//! Integration tests for PlainWeb network join: two live nodes on loopback
//! UDP can discover each other via connection links and exchange
//! `NetworkMessage` datagrams end-to-end.

use pwb_core::crypto::{ContentId, Keypair, Signature};
use pwb_core::net::{NetworkMessage, NodeConfig, NodeEngine};
use std::net::SocketAddr;
use std::time::Duration;

fn engine_config(keypair: Keypair) -> NodeConfig {
    NodeConfig {
        listen_addr: "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
        keypair,
        bootstrap_peers: Vec::new(),
        max_peers: 100,
        heartbeat_interval: Duration::from_secs(30),
    }
}

async fn start_engine(kp: Keypair) -> (NodeEngine, SocketAddr) {
    let engine = NodeEngine::new(engine_config(kp.clone()));
    engine.start().await.expect("node engine start");
    let addr = engine
        .local_addr()
        .expect("socket bound")
        .expect("local addr");
    (engine, addr)
}

#[tokio::test]
async fn test_two_nodes_exchange_peer_discovery_over_udp() {
    // Node A and Node B each bind a UDP socket and start the full stack.
    let kp_a = Keypair::generate();
    let (engine_a, addr_a) = start_engine(kp_a.clone()).await;

    let kp_b = Keypair::generate();
    let (engine_b, _) = start_engine(kp_b.clone()).await;

    // B learns A's connection link (out-of-band, e.g. QR code) and connects.
    let link = NodeEngine::build_connection_link(addr_a, kp_a.public);
    assert!(link.starts_with("plain://nodes.plain?connect="));
    engine_b
        .launch_connection(&link)
        .expect("B connects to A via link");

    // B sends a PeerDiscovery announcement to A over UDP.
    engine_b
        .send_to(
            &kp_a.public,
            NetworkMessage::PeerDiscovery {
                known_peers: Vec::new(),
                sender_pubkey: kp_b.public,
            },
        )
        .expect("B sends PeerDiscovery to A");

    // Give the receiver loop time to dispatch the message to A's handler.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // A should have learned about B from the inbound announcement.
    let peer = engine_a.get_peer(&kp_b.public);
    assert!(
        peer.is_some(),
        "Node A should have Node B in its peer table after receiving PeerDiscovery"
    );
    let peer = peer.unwrap();
    assert_eq!(peer.public_key, kp_b.public);

    // A can now send a message back to B (B is in A's peer table).
    engine_a
        .send_to(
            &kp_b.public,
            NetworkMessage::SearchQuery {
                query: "hello from A".to_string(),
                sender: kp_a.public,
            },
        )
        .expect("A sends SearchQuery to B");

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Both nodes should have each other in their peer tables.
    assert!(engine_b.get_peer(&kp_a.public).is_some(), "B should know A");
    assert!(engine_a.get_peer(&kp_b.public).is_some(), "A should know B");

    engine_a.stop().await;
    engine_b.stop().await;
}

#[tokio::test]
async fn test_broadcast_reaches_all_peers() {
    let kp_a = Keypair::generate();
    let (engine_a, addr_a) = start_engine(kp_a.clone()).await;

    let kp_b = Keypair::generate();
    let (engine_b, _) = start_engine(kp_b.clone()).await;

    let kp_c = Keypair::generate();
    let (engine_c, _) = start_engine(kp_c.clone()).await;

    // B and C both connect to A.
    let link_a = NodeEngine::build_connection_link(addr_a, kp_a.public);
    engine_b
        .launch_connection(&link_a)
        .expect("B connects to A");
    engine_c
        .launch_connection(&link_a)
        .expect("C connects to A");

    // A broadcasts a GossipAnnounce; B and C should both receive it.
    let cid = ContentId([7u8; 32]);
    engine_a
        .broadcast(NetworkMessage::GossipAnnounce {
            content_id: cid,
            title: "new artifact".to_string(),
            publisher: kp_a.public,
            signature: Signature([2u8; 64]),
        })
        .expect("A broadcasts");

    tokio::time::sleep(Duration::from_millis(200)).await;

    // B and C should have learned about A from the gossip announcement.
    assert!(
        engine_b.get_peer(&kp_a.public).is_some(),
        "B should have A after broadcast"
    );
    assert!(
        engine_c.get_peer(&kp_a.public).is_some(),
        "C should have A after broadcast"
    );
    assert_eq!(engine_b.get_peer(&kp_a.public).unwrap().address, addr_a);
    assert_eq!(engine_c.get_peer(&kp_a.public).unwrap().address, addr_a);

    engine_a.stop().await;
    engine_b.stop().await;
    engine_c.stop().await;
}

#[tokio::test]
async fn test_connection_link_roundtrip_across_nodes() {
    let kp_a = Keypair::generate();
    let (engine_a, addr_a) = start_engine(kp_a.clone()).await;

    let kp_b = Keypair::generate();
    let (engine_b, _) = start_engine(kp_b.clone()).await;

    // A generates its link; B parses and connects.
    let link = engine_a.generate_connection_link();
    let parsed = NodeEngine::parse_connection_link(&link).expect("link parses");
    assert_eq!(parsed.0, addr_a);
    assert_eq!(parsed.1, kp_a.public);

    let result = engine_b.launch_connection(&link);
    assert!(result.is_ok(), "B can connect to A via A's generated link");
    let peer = engine_b.get_peer(&kp_a.public).unwrap();
    assert_eq!(peer.address, addr_a);
    assert_eq!(peer.public_key, kp_a.public);

    engine_a.stop().await;
    engine_b.stop().await;
}

#[tokio::test]
async fn test_node_cannot_send_to_unknown_peer() {
    let kp_a = Keypair::generate();
    let (engine_a, _) = start_engine(kp_a.clone()).await;

    let stranger = Keypair::generate().public;
    let err = engine_a
        .send_to(
            &stranger,
            NetworkMessage::SearchQuery {
                query: "x".to_string(),
                sender: kp_a.public,
            },
        )
        .unwrap_err();
    assert!(
        matches!(err, pwb_core::net::NetworkError::PeerNotFound(_)),
        "sending to unknown peer must fail with PeerNotFound"
    );

    engine_a.stop().await;
}
