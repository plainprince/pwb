use super::*;
use crate::crypto::{ContentId, DhPublicKey, Keypair};
use crate::onion::{CoverConfig, OnionCell, OnionError, CELL_SIZE};
use std::net::SocketAddr;
use std::str::FromStr;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;

#[tokio::test]
async fn test_node_engine_creation() {
    let kp = Keypair::generate();
    let config = NodeConfig {
        listen_addr: SocketAddr::from_str("127.0.0.1:0").unwrap(),
        keypair: kp.clone(),
        bootstrap_peers: Vec::new(),
        stun_server: None,
        max_peers: 100,
        heartbeat_interval: Duration::from_secs(30),
    };

    let engine = NodeEngine::new(config);
    assert!(!*engine.running.lock().unwrap());

    // Start the engine
    let result = engine.start().await;
    assert!(result.is_ok());
    assert!(*engine.running.lock().unwrap());

    // Stop the engine
    engine.stop().await;
    assert!(!*engine.running.lock().unwrap());
}

#[tokio::test]
async fn test_peer_management() {
    let config = NodeConfig {
        listen_addr: SocketAddr::from_str("127.0.0.1:0").unwrap(),
        keypair: Keypair::generate(),
        bootstrap_peers: Vec::new(),
        stun_server: None,
        max_peers: 100,
        heartbeat_interval: Duration::from_secs(30),
    };

    let engine = NodeEngine::new(config);
    let _ = engine.start().await;

    // Add a peer
    let peer_kp = Keypair::generate();
    let peer_info = PeerInfo {
        public_key: peer_kp.public,
        last_seen: Instant::now(),
        address: SocketAddr::from_str("127.0.0.1:8080").unwrap(),
        reputation: 5,
    };

    engine.add_peer(peer_info);
    let retrieved = engine.get_peer(&peer_kp.public);
    assert!(retrieved.is_some());
    assert_eq!(retrieved.unwrap().reputation, 5);

    // Remove peer
    engine.remove_peer(&peer_kp.public);
    let retrieved_after = engine.get_peer(&peer_kp.public);
    assert!(retrieved_after.is_none());

    engine.stop().await;
}

#[tokio::test]
async fn test_send_and_broadcast() {
    let kp = Keypair::generate();
    let config = NodeConfig {
        listen_addr: SocketAddr::from_str("127.0.0.1:0").unwrap(),
        keypair: kp.clone(),
        bootstrap_peers: Vec::new(),
        stun_server: None,
        max_peers: 100,
        heartbeat_interval: Duration::from_secs(30),
    };

    let engine = NodeEngine::new(config);
    let _ = engine.start().await;

    // Add a peer so we can send to it
    let peer_kp = Keypair::generate();
    let peer_info = PeerInfo {
        public_key: peer_kp.public,
        last_seen: Instant::now(),
        address: SocketAddr::from_str("127.0.0.1:8080").unwrap(),
        reputation: 5,
    };
    engine.add_peer(peer_info);

    // Test sending a message
    let message = NetworkMessage::SearchQuery {
        query: "test".to_string(),
        sender: kp.public,
    };

    let send_result = engine.send_to(&peer_kp.public, message.clone());
    assert!(send_result.is_ok());

    // Test broadcasting
    let broadcast_result = engine.broadcast(message);
    assert!(broadcast_result.is_ok());

    engine.stop().await;
}

#[tokio::test]
async fn test_message_processing_peer_discovery() {
    let kp_a = Keypair::generate();
    let engine_a = NodeEngine::new(NodeConfig {
        listen_addr: SocketAddr::from_str("127.0.0.1:0").unwrap(),
        keypair: kp_a.clone(),
        bootstrap_peers: Vec::new(),
        stun_server: None,
        max_peers: 100,
        heartbeat_interval: Duration::from_secs(30),
    });
    engine_a.start().await.expect("A starts");
    let addr_a = engine_a.local_addr().unwrap().unwrap();

    let kp_b = Keypair::generate();
    let engine_b = NodeEngine::new(NodeConfig {
        listen_addr: SocketAddr::from_str("127.0.0.1:0").unwrap(),
        keypair: kp_b.clone(),
        bootstrap_peers: Vec::new(),
        stun_server: None,
        max_peers: 100,
        heartbeat_interval: Duration::from_secs(30),
    });
    engine_b.start().await.expect("B starts");
    let addr_b = engine_b.local_addr().unwrap().unwrap();

    engine_b.connect_to(addr_a, kp_a.public).expect("B knows A");
    engine_b
        .send_to(
            &kp_a.public,
            NetworkMessage::PeerDiscovery {
                known_peers: Vec::new(),
                sender_pubkey: kp_b.public,
            },
        )
        .expect("B sends PeerDiscovery to A");

    tokio::time::sleep(Duration::from_millis(150)).await;

    // A attributes the datagram to the key that signed it and uses the real
    // source address — never the destination address the sender wrote to.
    let peer = engine_a
        .get_peer(&kp_b.public)
        .expect("A learned B from B's signed datagram");
    assert_eq!(peer.public_key, kp_b.public);
    assert_eq!(peer.address, addr_b);

    // A never registers itself, even though it sent the original message.
    assert!(
        engine_a.get_peer(&kp_a.public).is_none(),
        "a node must never appear in its own peer table"
    );

    engine_a.stop().await;
    engine_b.stop().await;
}

#[tokio::test]
async fn test_message_processing_with_known_peers() {
    let kp_a = Keypair::generate();
    let engine_a = NodeEngine::new(NodeConfig {
        listen_addr: SocketAddr::from_str("127.0.0.1:0").unwrap(),
        keypair: kp_a.clone(),
        bootstrap_peers: Vec::new(),
        stun_server: None,
        max_peers: 100,
        heartbeat_interval: Duration::from_secs(30),
    });
    engine_a.start().await.expect("A starts");
    let addr_a = engine_a.local_addr().unwrap().unwrap();

    let kp_b = Keypair::generate();
    let engine_b = NodeEngine::new(NodeConfig {
        listen_addr: SocketAddr::from_str("127.0.0.1:0").unwrap(),
        keypair: kp_b.clone(),
        bootstrap_peers: Vec::new(),
        stun_server: None,
        max_peers: 100,
        heartbeat_interval: Duration::from_secs(30),
    });
    engine_b.start().await.expect("B starts");

    // Keys with no dialable address must not be registered at the sender's
    // address: doing so tells the network that someone else's key lives here.
    let peer1_kp = Keypair::generate();
    let peer2_kp = Keypair::generate();
    engine_b.connect_to(addr_a, kp_a.public).expect("B knows A");
    engine_b
        .send_to(
            &kp_a.public,
            NetworkMessage::PeerDiscovery {
                known_peers: vec![peer1_kp.public, peer2_kp.public],
                sender_pubkey: kp_b.public,
            },
        )
        .expect("B sends PeerDiscovery to A");

    tokio::time::sleep(Duration::from_millis(150)).await;

    assert!(engine_a.get_peer(&kp_b.public).is_some());
    assert!(
        engine_a.get_peer(&peer1_kp.public).is_none(),
        "address-less peers are not registered at the sender's address"
    );
    assert!(engine_a.get_peer(&peer2_kp.public).is_none());
    assert!(engine_a.get_peer(&kp_a.public).is_none());

    engine_a.stop().await;
    engine_b.stop().await;
}

/// Test generating and parsing a connection link.
#[tokio::test]
async fn test_connection_link_generation_and_parsing() {
    // Create a NodeEngine with a known keypair
    let kp = Keypair::generate();
    let config = NodeConfig {
        listen_addr: SocketAddr::from_str("127.0.0.1:8080").unwrap(),
        keypair: kp.clone(),
        bootstrap_peers: Vec::new(),
        stun_server: None,
        max_peers: 100,
        heartbeat_interval: Duration::from_secs(30),
    };

    let engine = NodeEngine::new(config);

    // Generate the connection link
    let link = engine.generate_connection_link();
    assert!(link.starts_with("plain://nodes.plain?connect="));

    // Parse the link back
    let parsed = NodeEngine::parse_connection_link(&link).expect("Failed to parse link");
    assert_eq!(parsed.0, SocketAddr::from_str("127.0.0.1:8080").unwrap());
    assert_eq!(parsed.1, kp.public);
}

/// Test parsing an invalid connection link.
#[tokio::test]
async fn test_connection_link_parsing_invalid() {
    // Test various invalid links
    assert!(NodeEngine::parse_connection_link("").is_none());
    assert!(NodeEngine::parse_connection_link("plain://nodes.plain?connect=").is_none());
    assert!(
        NodeEngine::parse_connection_link("plain://nodes.plain?connect=127.0.0.1:8080/").is_none()
    );
    assert!(
        NodeEngine::parse_connection_link("plain://nodes.plain?connect=127.0.0.1:8080/short")
            .is_none()
    );
    assert!(NodeEngine::parse_connection_link("wrong://nodes.plain?connect=127.0.0.1:8080/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef").is_none());
    assert!(NodeEngine::parse_connection_link("plain://search.plain?connect=127.0.0.1:8080/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef").is_none());
}

/// Test that two nodes with different keypairs generate compatible connection links.
#[tokio::test]
async fn test_multi_node_link_compatibility() {
    use crate::crypto::Keypair;
    use crate::net::NodeEngine;

    // Node 1 generates its link with a known keypair
    let kp1 = Keypair::generate();
    let config1 = NodeConfig {
        listen_addr: "192.168.1.1:8081".parse::<SocketAddr>().unwrap(),
        keypair: kp1.clone(),
        bootstrap_peers: Vec::new(),
        stun_server: None,
        max_peers: 100,
        heartbeat_interval: Duration::from_secs(30),
    };
    let engine1 = NodeEngine::new(config1);
    let link1 = engine1.generate_connection_link();

    // Node 2 generates its link with a different keypair
    let kp2 = Keypair::generate();
    let config2 = NodeConfig {
        listen_addr: "192.168.1.2:8082".parse::<SocketAddr>().unwrap(),
        keypair: kp2.clone(),
        bootstrap_peers: Vec::new(),
        stun_server: None,
        max_peers: 100,
        heartbeat_interval: Duration::from_secs(30),
    };
    let engine2 = NodeEngine::new(config2);
    let link2 = engine2.generate_connection_link();

    // Both links should start with the correct prefix
    assert!(link1.starts_with("plain://nodes.plain?connect="));
    assert!(link2.starts_with("plain://nodes.plain?connect="));

    // Each link contains its own address
    let parsed1 = NodeEngine::parse_connection_link(&link1).expect("Failed to parse link1");
    let parsed2 = NodeEngine::parse_connection_link(&link2).expect("Failed to parse link2");

    // Node 1's link should parse to its own address
    assert_eq!(parsed1.0, "192.168.1.1:8081".parse::<SocketAddr>().unwrap());
    // Node 2's link should parse to its own address
    assert_eq!(parsed2.0, "192.168.1.2:8082".parse::<SocketAddr>().unwrap());

    // The addresses should be different (different nodes on different IPs/ports)
    assert_ne!(parsed1.0, parsed2.0);

    // Now test parse+generate roundtrip for both
    let link1_back = NodeEngine::generate_connection_link_from_parsed(parsed1);
    let link2_back = NodeEngine::generate_connection_link_from_parsed(parsed2);

    assert_eq!(link1, link1_back);
    assert_eq!(link2, link2_back);
}

/// TODO §2.3 source-IP masking: three relays on loopback UDP. Each hop
/// re-originates the datagram from its own socket, so every receiver
/// observes only the previous relay's address — the originator's IP
/// stops at the entry hop and the envelope (exactly the cell bytes)
/// carries no source field that could resurrect it.
#[tokio::test]
async fn test_cell_channel_masks_source_ip_across_hops() {
    use crate::crypto::DhKeypair;
    use crate::onion::OnionAction;

    let relays: Vec<DhKeypair> = (0..3).map(|_| DhKeypair::generate()).collect();

    let originator = CellChannel::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let mut entry = CellChannel::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let mut middle = CellChannel::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let exit = CellChannel::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();

    let originator_addr = originator.local_addr().unwrap();
    let entry_addr = entry.local_addr().unwrap();
    let middle_addr = middle.local_addr().unwrap();
    let exit_addr = exit.local_addr().unwrap();

    entry.register_relay(relays[1].public, middle_addr);
    middle.register_relay(relays[2].public, exit_addr);

    let path = [relays[0].public, relays[1].public, relays[2].public];
    let cell = OnionCell::encapsulate(b"hidden query", &path).unwrap();

    // Hop 1: only the entry ever sees the originator's address.
    originator.send_cell_to(entry_addr, &cell).await.unwrap();
    let (cell, from) = entry.recv_cell().await.unwrap();
    assert_eq!(from, originator_addr);

    // The entry peels and re-originates toward the middle from *its own*
    // socket.
    let peeled = cell.peel(&relays[0]).unwrap().unwrap();
    let OnionAction::Forward(next) = peeled.action else {
        panic!("entry must forward, got {:?}", peeled.action);
    };
    assert_eq!(next, relays[1].public);
    entry
        .send_cell(&next, &peeled.forward.unwrap())
        .await
        .unwrap();

    // Hop 2: only the entry's IP is observable — the originator's
    // address does not survive the entry hop.
    let (cell, from) = middle.recv_cell().await.unwrap();
    assert_eq!(from, entry_addr, "hop 2 sees only the previous relay");
    assert_ne!(from, originator_addr, "originator IP must not reach hop 2");

    let peeled = cell.peel(&relays[1]).unwrap().unwrap();
    let OnionAction::Forward(next) = peeled.action else {
        panic!("middle must forward, got {:?}", peeled.action);
    };
    middle
        .send_cell(&next, &peeled.forward.unwrap())
        .await
        .unwrap();

    // Hop 3: the exit sees only the middle's IP and recovers the payload.
    let (cell, from) = exit.recv_cell().await.unwrap();
    assert_eq!(from, middle_addr);
    let peeled = cell.peel(&relays[2]).unwrap().unwrap();
    assert_eq!(peeled.action, OnionAction::Deliver);
    assert_eq!(peeled.payload.as_deref(), Some(b"hidden query".as_slice()));
}

/// TODO §2.3: unknown next hops are refused before anything is sent, and
/// malformed / wrong-version datagrams are rejected on receive.
#[tokio::test]
async fn test_cell_channel_rejects_unknown_relay_and_bad_datagrams() {
    use crate::crypto::DhKeypair;
    use crate::onion::ONION_VERSION;

    let mut channel = CellChannel::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let keys: Vec<DhKeypair> = (0..3).map(|_| DhKeypair::generate()).collect();
    let path = [keys[0].public, keys[1].public, keys[2].public];
    let cell = OnionCell::encapsulate(b"x", &path).unwrap();

    let raw = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let raw_addr = raw.local_addr().unwrap();
    let addr = channel.local_addr().unwrap();

    // Unregistered relay → PeerNotFound, nothing leaves the socket.
    let unknown = DhKeypair::generate().public;
    let err = channel.send_cell(&unknown, &cell).await.unwrap_err();
    assert!(matches!(err, NetworkError::PeerNotFound(_)));

    // Registered relay → address lookup round-trips and send succeeds.
    channel.register_relay(unknown, raw_addr);
    assert_eq!(channel.relay_address(&unknown), Some(raw_addr));
    assert_eq!(channel.relay_address(&DhKeypair::generate().public), None);
    channel.send_cell(&unknown, &cell).await.unwrap();

    // Undersized datagram → InvalidMessage.
    raw.send_to(b"not a cell", addr).await.unwrap();
    assert!(matches!(
        channel.recv_cell().await,
        Err(NetworkError::InvalidMessage)
    ));

    // Correct size but wrong onion version → typed onion error.
    let mut bytes = cell.to_bytes();
    bytes[0] = ONION_VERSION + 1;
    raw.send_to(&bytes, addr).await.unwrap();
    let err = channel.recv_cell().await.unwrap_err();
    assert!(matches!(
        err,
        NetworkError::Onion(OnionError::UnsupportedVersion(_))
    ));
}

/// TODO §2.3 cover traffic: a scheduled decoy rides the same masked hop
/// channel as real cells — constant-size datagram, peelable by the
/// first relay, dispatched exactly on the scheduler's due time.
#[tokio::test]
async fn test_cover_decoy_dispatched_over_cell_channel() {
    use crate::crypto::DhKeypair;
    use crate::onion::{CoverConfig, CoverDispatch, OnionAction};
    use rand::{rngs::StdRng, SeedableRng};

    let keys: Vec<DhKeypair> = (0..3).map(|_| DhKeypair::generate()).collect();
    let candidates: Vec<DhPublicKey> = keys.iter().map(|k| k.public).collect();

    let mut rng = StdRng::seed_from_u64(21);
    let mut dispatch = CoverDispatch::new(CoverConfig {
        mean_interval: Duration::from_secs(60),
        min_interval: Duration::from_secs(1),
        max_interval: Duration::from_secs(120),
    });

    // Arm, then collect the decoy exactly when due.
    let t0 = Instant::now();
    assert!(dispatch.poll(t0, &candidates, &mut rng).is_none());
    let due = dispatch.next_due().expect("armed");
    let (decoy, path) = dispatch
        .poll(due, &candidates, &mut rng)
        .expect("due poll dispatches a decoy");

    // Send it like a real request: sender socket → first relay's socket.
    let mut sender = CellChannel::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let hop = CellChannel::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let hop_addr = hop.local_addr().unwrap();
    sender.register_relay(path.relays[0], hop_addr);
    sender.send_cell(&path.relays[0], &decoy).await.unwrap();

    let (cell, from) = hop.recv_cell().await.unwrap();
    assert_eq!(from, sender.local_addr().unwrap());
    assert_eq!(
        cell.to_bytes().len(),
        CELL_SIZE,
        "cover datagrams are indistinguishable in size"
    );

    // The receiving relay peels the decoy exactly like a real request.
    let key_for = |pk: &DhPublicKey| {
        keys.iter()
            .find(|k| k.public == *pk)
            .expect("path relay comes from candidates")
    };
    let peeled = cell.peel(key_for(&path.relays[0])).unwrap().unwrap();
    let OnionAction::Forward(next) = peeled.action else {
        panic!("first hop of a decoy path must forward");
    };
    assert_eq!(next, path.relays[1]);
}

/// Engine error paths: double-start is a no-op, `peer_table()` exposes
/// the shared table, unknown-peer sends fail before touching the
/// channel, and inbound traffic registers/refreshes the authenticated
/// sender without ever registering the local node itself.
#[tokio::test]
async fn test_node_engine_error_paths_and_gossip_registration() {
    let kp_a = Keypair::generate();
    let engine_a = NodeEngine::new(NodeConfig {
        listen_addr: SocketAddr::from_str("127.0.0.1:0").unwrap(),
        keypair: kp_a.clone(),
        bootstrap_peers: Vec::new(),
        stun_server: None,
        max_peers: 100,
        heartbeat_interval: Duration::from_secs(30),
    });

    engine_a.start().await.expect("first start");
    engine_a.start().await.expect("second start is a no-op");
    assert!(
        engine_a.peer_table().lock().unwrap().is_empty(),
        "no peers registered before any traffic"
    );

    // Unknown peer → PeerNotFound before anything is queued.
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
    assert!(matches!(err, NetworkError::PeerNotFound(_)));

    let addr_a = engine_a.local_addr().unwrap().unwrap();
    let kp_b = Keypair::generate();
    let engine_b = NodeEngine::new(NodeConfig {
        listen_addr: SocketAddr::from_str("127.0.0.1:0").unwrap(),
        keypair: kp_b.clone(),
        bootstrap_peers: Vec::new(),
        stun_server: None,
        max_peers: 100,
        heartbeat_interval: Duration::from_secs(30),
    });
    engine_b.start().await.expect("B starts");
    engine_b.connect_to(addr_a, kp_a.public).expect("B knows A");

    // A gossip announcement from B registers B and stamps `last_seen`.
    engine_b
        .send_to(
            &kp_a.public,
            NetworkMessage::gossip_announce(&kp_b, ContentId([1u8; 32]), "artifact"),
        )
        .expect("B sends gossip");
    tokio::time::sleep(Duration::from_millis(100)).await;
    let first = engine_a.get_peer(&kp_b.public).expect("B registered");
    assert_eq!(first.public_key, kp_b.public);

    // A later announcement refreshes the existing entry rather than
    // registering a duplicate.
    tokio::time::sleep(Duration::from_millis(30)).await;
    engine_b
        .send_to(
            &kp_a.public,
            NetworkMessage::gossip_announce(&kp_b, ContentId([3u8; 32]), "artifact 2"),
        )
        .expect("B sends gossip");
    tokio::time::sleep(Duration::from_millis(100)).await;
    let second = engine_a.get_peer(&kp_b.public).expect("B still registered");
    assert!(second.last_seen > first.last_seen, "last_seen refreshed");
    assert!(engine_a.get_peer(&kp_a.public).is_none());

    engine_a.stop().await;
    engine_b.stop().await;
}

#[tokio::test]
async fn test_launch_connection_valid_link() {
    let kp = Keypair::generate();
    let config = NodeConfig {
        listen_addr: SocketAddr::from_str("127.0.0.1:0").unwrap(),
        keypair: kp.clone(),
        bootstrap_peers: Vec::new(),
        stun_server: None,
        max_peers: 100,
        heartbeat_interval: Duration::from_secs(30),
    };
    let engine = NodeEngine::new(config);
    let _ = engine.start().await;
    let peer_kp = Keypair::generate();
    let link = format!(
        "plain://nodes.plain?connect=127.0.0.1:9000/{}",
        peer_kp.public.to_hex()
    );
    let result = engine.launch_connection(&link);
    assert!(result.is_ok());
    let peer = engine.get_peer(&peer_kp.public).unwrap();
    assert_eq!(
        peer.address,
        "127.0.0.1:9000".parse::<SocketAddr>().unwrap()
    );
    assert_eq!(peer.public_key, peer_kp.public);
    engine.stop().await;
}

#[tokio::test]
async fn test_launch_connection_invalid_link() {
    let kp = Keypair::generate();
    let config = NodeConfig {
        listen_addr: SocketAddr::from_str("127.0.0.1:0").unwrap(),
        keypair: kp.clone(),
        bootstrap_peers: Vec::new(),
        stun_server: None,
        max_peers: 100,
        heartbeat_interval: Duration::from_secs(30),
    };
    let engine = NodeEngine::new(config);
    let _ = engine.start().await;
    let result = engine.launch_connection("garbage-link");
    assert!(matches!(
        result,
        Err(NetworkError::InvalidConnectionLink(_))
    ));
    engine.stop().await;
}

#[tokio::test]
async fn test_launch_connection_self_link() {
    let kp = Keypair::generate();
    let config = NodeConfig {
        listen_addr: SocketAddr::from_str("127.0.0.1:0").unwrap(),
        keypair: kp.clone(),
        bootstrap_peers: Vec::new(),
        stun_server: None,
        max_peers: 100,
        heartbeat_interval: Duration::from_secs(30),
    };
    let engine = NodeEngine::new(config);
    let _ = engine.start().await;
    let link = format!(
        "plain://nodes.plain?connect=127.0.0.1:9000/{}",
        kp.public.to_hex()
    );
    let result = engine.launch_connection(&link);
    assert!(matches!(
        result,
        Err(NetworkError::InvalidConnectionLink(_))
    ));
    engine.stop().await;
}

#[tokio::test]
async fn test_launch_connection_port_zero() {
    let kp = Keypair::generate();
    let config = NodeConfig {
        listen_addr: SocketAddr::from_str("127.0.0.1:0").unwrap(),
        keypair: kp.clone(),
        bootstrap_peers: Vec::new(),
        stun_server: None,
        max_peers: 100,
        heartbeat_interval: Duration::from_secs(30),
    };
    let engine = NodeEngine::new(config);
    let _ = engine.start().await;
    let peer_kp = Keypair::generate();
    let link = format!(
        "plain://nodes.plain?connect=127.0.0.1:0/{}",
        peer_kp.public.to_hex()
    );
    let result = engine.launch_connection(&link);
    assert!(matches!(
        result,
        Err(NetworkError::InvalidConnectionLink(_))
    ));
    engine.stop().await;
}

#[tokio::test]
async fn test_launch_connection_unspecified_address() {
    let kp = Keypair::generate();
    let config = NodeConfig {
        listen_addr: SocketAddr::from_str("127.0.0.1:0").unwrap(),
        keypair: kp.clone(),
        bootstrap_peers: Vec::new(),
        stun_server: None,
        max_peers: 100,
        heartbeat_interval: Duration::from_secs(30),
    };
    let engine = NodeEngine::new(config);
    let _ = engine.start().await;
    let peer_kp = Keypair::generate();
    let link = format!(
        "plain://nodes.plain?connect=0.0.0.0:8080/{}",
        peer_kp.public.to_hex()
    );
    let result = engine.launch_connection(&link);
    assert!(matches!(
        result,
        Err(NetworkError::InvalidConnectionLink(_))
    ));
    engine.stop().await;
}

#[tokio::test]
async fn test_launch_connection_refresh_preserves_reputation() {
    let kp = Keypair::generate();
    let config = NodeConfig {
        listen_addr: SocketAddr::from_str("127.0.0.1:0").unwrap(),
        keypair: kp.clone(),
        bootstrap_peers: Vec::new(),
        stun_server: None,
        max_peers: 100,
        heartbeat_interval: Duration::from_secs(30),
    };
    let engine = NodeEngine::new(config);
    let _ = engine.start().await;
    let peer_kp = Keypair::generate();
    let peer_info = PeerInfo {
        public_key: peer_kp.public,
        last_seen: Instant::now(),
        address: "127.0.0.1:8080".parse::<SocketAddr>().unwrap(),
        reputation: 42,
    };
    engine.add_peer(peer_info);
    let link = format!(
        "plain://nodes.plain?connect=127.0.0.1:9000/{}",
        peer_kp.public.to_hex()
    );
    let result = engine.launch_connection(&link);
    assert!(result.is_ok());
    let final_peer = engine.get_peer(&peer_kp.public).unwrap();
    assert_eq!(final_peer.reputation, 42);
    assert_eq!(
        final_peer.address,
        "127.0.0.1:9000".parse::<SocketAddr>().unwrap()
    );
    engine.stop().await;
}

#[tokio::test]
async fn test_connect_to_valid() {
    let kp = Keypair::generate();
    let config = NodeConfig {
        listen_addr: SocketAddr::from_str("127.0.0.1:0").unwrap(),
        keypair: kp.clone(),
        bootstrap_peers: Vec::new(),
        stun_server: None,
        max_peers: 100,
        heartbeat_interval: Duration::from_secs(30),
    };
    let engine = NodeEngine::new(config);
    let _ = engine.start().await;
    let peer_kp = Keypair::generate();
    let addr = "127.0.0.1:9000".parse::<SocketAddr>().unwrap();
    let result = engine.connect_to(addr, peer_kp.public);
    assert!(result.is_ok());
    let peer = engine.get_peer(&peer_kp.public).unwrap();
    assert_eq!(peer.address, addr);
    assert_eq!(peer.public_key, peer_kp.public);
    engine.stop().await;
}

#[tokio::test]
async fn test_connect_to_invalid_address() {
    let kp = Keypair::generate();
    let config = NodeConfig {
        listen_addr: SocketAddr::from_str("127.0.0.1:0").unwrap(),
        keypair: kp.clone(),
        bootstrap_peers: Vec::new(),
        stun_server: None,
        max_peers: 100,
        heartbeat_interval: Duration::from_secs(30),
    };
    let engine = NodeEngine::new(config);
    let _ = engine.start().await;
    let peer_kp = Keypair::generate();
    let addr = "0.0.0.0:8080".parse::<SocketAddr>().unwrap();
    let result = engine.connect_to(addr, peer_kp.public);
    assert!(matches!(
        result,
        Err(NetworkError::InvalidConnectionLink(_))
    ));
    engine.stop().await;
}

#[tokio::test]
async fn test_connect_to_self_rejection() {
    let kp = Keypair::generate();
    let config = NodeConfig {
        listen_addr: SocketAddr::from_str("127.0.0.1:0").unwrap(),
        keypair: kp.clone(),
        bootstrap_peers: Vec::new(),
        stun_server: None,
        max_peers: 100,
        heartbeat_interval: Duration::from_secs(30),
    };
    let engine = NodeEngine::new(config);
    let _ = engine.start().await;
    let addr = "127.0.0.1:8080".parse::<SocketAddr>().unwrap();
    let result = engine.connect_to(addr, kp.public);
    assert!(matches!(
        result,
        Err(NetworkError::InvalidConnectionLink(_))
    ));
    engine.stop().await;
}

#[tokio::test]
async fn test_connect_to_port_zero_rejection() {
    let kp = Keypair::generate();
    let config = NodeConfig {
        listen_addr: SocketAddr::from_str("127.0.0.1:0").unwrap(),
        keypair: kp.clone(),
        bootstrap_peers: Vec::new(),
        stun_server: None,
        max_peers: 100,
        heartbeat_interval: Duration::from_secs(30),
    };
    let engine = NodeEngine::new(config);
    let _ = engine.start().await;
    let peer_kp = Keypair::generate();
    let addr = "127.0.0.1:0".parse::<SocketAddr>().unwrap();
    let result = engine.connect_to(addr, peer_kp.public);
    assert!(matches!(
        result,
        Err(NetworkError::InvalidConnectionLink(_))
    ));
    engine.stop().await;
}

#[tokio::test]
async fn test_connect_to_refresh_preserves_reputation() {
    let kp = Keypair::generate();
    let config = NodeConfig {
        listen_addr: SocketAddr::from_str("127.0.0.1:0").unwrap(),
        keypair: kp.clone(),
        bootstrap_peers: Vec::new(),
        stun_server: None,
        max_peers: 100,
        heartbeat_interval: Duration::from_secs(30),
    };
    let engine = NodeEngine::new(config);
    let _ = engine.start().await;
    let peer_kp = Keypair::generate();
    let addr1 = "127.0.0.1:8080".parse::<SocketAddr>().unwrap();
    let result1 = engine.connect_to(addr1, peer_kp.public);
    assert!(result1.is_ok());
    let info = engine.get_peer(&peer_kp.public).unwrap();
    assert_eq!(info.address, addr1);
    assert_eq!(info.reputation, 0);
    let addr2 = "127.0.0.1:9000".parse::<SocketAddr>().unwrap();
    let result2 = engine.connect_to(addr2, peer_kp.public);
    assert!(result2.is_ok());
    let final_info = engine.get_peer(&peer_kp.public).unwrap();
    assert_eq!(final_info.address, addr2);
    assert_eq!(final_info.reputation, 0);
    engine.stop().await;
}

/// Setting `public_addr` (via STUN) makes `generate_connection_link` advertise
/// the public address instead of the local bound address — NAT traversal so
/// peers on the internet can dial the link.
#[tokio::test]
async fn test_connection_link_uses_public_addr_when_stun_available() {
    let kp = Keypair::generate();
    let config = NodeConfig {
        listen_addr: "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
        keypair: kp.clone(),
        bootstrap_peers: Vec::new(),
        stun_server: None,
        max_peers: 100,
        heartbeat_interval: Duration::from_secs(30),
    };
    let engine = NodeEngine::new(config);
    let _ = engine.start().await;

    let local_addr = engine.local_addr().expect("bound").expect("addr");
    let link_before = engine.generate_connection_link();
    let parsed_before = NodeEngine::parse_connection_link(&link_before).unwrap();
    assert_eq!(
        parsed_before.0, local_addr,
        "link uses local addr before STUN"
    );

    // Simulate STUN having discovered a public address.
    let public_addr: SocketAddr = "203.0.113.45:19302".parse().unwrap();
    *engine.public_addr.lock().unwrap() = Some(public_addr);

    let link_after = engine.generate_connection_link();
    let parsed_after = NodeEngine::parse_connection_link(&link_after).unwrap();
    assert_eq!(
        parsed_after.0, public_addr,
        "link uses public addr after STUN"
    );
    assert_ne!(
        parsed_after.0, local_addr,
        "public addr differs from local addr"
    );

    engine.stop().await;
}

/// `connect_bootstrap` dials every configured bootstrap peer and registers them
/// in the peer table — this is how a new node joins the swarm without an
/// out-of-band introduction.
#[tokio::test]
async fn test_connect_bootstrap_registers_peers() {
    let kp = Keypair::generate();
    let bootstrap_kp = Keypair::generate();
    let config = NodeConfig {
        listen_addr: "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
        keypair: kp.clone(),
        bootstrap_peers: vec![BootstrapPeer {
            address: "127.0.0.1:9000".parse().unwrap(),
            public_key: bootstrap_kp.public,
        }],
        stun_server: None,
        max_peers: 100,
        heartbeat_interval: Duration::from_secs(30),
    };
    let engine = NodeEngine::new(config);
    let _ = engine.start().await;

    let connected = engine.connect_bootstrap();
    assert_eq!(connected.len(), 1, "connected to one bootstrap peer");
    assert_eq!(connected[0].public_key, bootstrap_kp.public);
    assert_eq!(
        connected[0].address,
        "127.0.0.1:9000".parse::<SocketAddr>().unwrap()
    );

    let peer = engine.get_peer(&bootstrap_kp.public).unwrap();
    assert_eq!(
        peer.address,
        "127.0.0.1:9000".parse::<SocketAddr>().unwrap()
    );

    engine.stop().await;
}

/// `PeerExchange` gossip adds advertised peers to the peer table using their
/// advertised addresses, so bootstrap/rendezvous nodes can teach newcomers
/// about the rest of the swarm.
#[tokio::test]
async fn test_peer_exchange_adds_peers_with_advertised_addresses() {
    let kp_a = Keypair::generate();
    let engine_a = NodeEngine::new(NodeConfig {
        listen_addr: "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
        keypair: kp_a.clone(),
        bootstrap_peers: Vec::new(),
        stun_server: None,
        max_peers: 100,
        heartbeat_interval: Duration::from_secs(30),
    });
    engine_a.start().await.expect("A starts");
    let addr_a = engine_a.local_addr().unwrap().unwrap();

    // A second node receives the exchange; A must not teach itself.
    let kp_b = Keypair::generate();
    let engine_b = NodeEngine::new(NodeConfig {
        listen_addr: "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
        keypair: kp_b.clone(),
        bootstrap_peers: Vec::new(),
        stun_server: None,
        max_peers: 100,
        heartbeat_interval: Duration::from_secs(30),
    });
    engine_b.start().await.expect("B starts");
    engine_b.connect_to(addr_a, kp_a.public).expect("B knows A");

    let peer1 = Keypair::generate();
    let peer2 = Keypair::generate();
    let addr1: SocketAddr = "10.0.0.1:5000".parse().unwrap();
    let addr2: SocketAddr = "10.0.0.2:6000".parse().unwrap();

    engine_b
        .send_to(
            &kp_a.public,
            NetworkMessage::PeerExchange {
                peers: vec![(addr1, peer1.public), (addr2, peer2.public)],
                sender_pubkey: kp_b.public,
            },
        )
        .expect("B sends exchange to A");

    tokio::time::sleep(Duration::from_millis(150)).await;

    let p1 = engine_a.get_peer(&peer1.public).expect("peer1 learned");
    assert_eq!(p1.address, addr1, "peer1 uses advertised address");
    let p2 = engine_a.get_peer(&peer2.public).expect("peer2 learned");
    assert_eq!(p2.address, addr2, "peer2 uses advertised address");
    assert!(engine_a.get_peer(&kp_a.public).is_none());

    engine_a.stop().await;
    engine_b.stop().await;
}

/// The heartbeat is a real liveness probe, not just a timer: a peer that
/// answers a Ping has its `last_seen` refreshed, so an actively talking peer
/// never expires from the table.
#[tokio::test]
async fn test_heartbeat_refreshes_peer_liveness() {
    let kp_b = Keypair::generate();
    let engine_b = NodeEngine::new(NodeConfig {
        listen_addr: SocketAddr::from_str("127.0.0.1:0").unwrap(),
        keypair: kp_b.clone(),
        bootstrap_peers: Vec::new(),
        stun_server: None,
        max_peers: 100,
        heartbeat_interval: Duration::from_millis(80),
    });
    engine_b.start().await.expect("B starts");
    let addr_b = engine_b.local_addr().unwrap().unwrap();

    let kp_a = Keypair::generate();
    let engine_a = NodeEngine::new(NodeConfig {
        listen_addr: SocketAddr::from_str("127.0.0.1:0").unwrap(),
        keypair: kp_a.clone(),
        bootstrap_peers: Vec::new(),
        stun_server: None,
        max_peers: 100,
        heartbeat_interval: Duration::from_millis(80),
    });
    engine_a.start().await.expect("A starts");
    engine_a.connect_to(addr_b, kp_b.public).expect("A knows B");

    let before = engine_a.get_peer(&kp_b.public).unwrap().last_seen;

    // Wait for at least one Ping/Pong round trip to refresh B.
    let mut refreshed = false;
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if engine_a
            .get_peer(&kp_b.public)
            .is_some_and(|peer| peer.last_seen > before)
        {
            refreshed = true;
            break;
        }
    }
    assert!(refreshed, "Pong from a live peer refreshes last_seen");

    engine_a.stop().await;
    engine_b.stop().await;
}

/// Datagrams that are not signed, are signed by the wrong key for the
/// identity they claim, or use the old unsigned JSON framing never reach the
/// peer table or the application.
#[tokio::test]
async fn test_forged_and_impersonating_datagrams_are_dropped() {
    let victim_engine_key = Keypair::generate();
    let victim_dh = crate::crypto::DhKeypair::from_ed25519(&victim_engine_key);
    let engine = NodeEngine::new(NodeConfig {
        listen_addr: SocketAddr::from_str("127.0.0.1:0").unwrap(),
        keypair: victim_engine_key,
        bootstrap_peers: Vec::new(),
        stun_server: None,
        max_peers: 100,
        heartbeat_interval: Duration::from_secs(30),
    });
    engine.start().await.expect("engine starts");
    let addr = engine.local_addr().unwrap().unwrap();

    let victim = Keypair::generate();
    let attacker = Keypair::generate();
    let raw = UdpSocket::bind("127.0.0.1:0").await.unwrap();

    // Legacy unsigned framing is not accepted at all.
    let unsigned = serde_json::to_vec(&NetworkMessage::SearchQuery {
        query: "plaintext injection".to_string(),
        sender: victim.public,
    })
    .unwrap();
    raw.send_to(&unsigned, addr).await.unwrap();

    // A signed message whose payload claims to be someone else is rejected.
    let impersonating = serde_json::to_vec(&NetworkMessage::SearchQuery {
        query: "impersonation".to_string(),
        sender: victim.public,
    })
    .unwrap();
    for datagram in super::envelope::encode_for(
        &attacker,
        &victim_dh.public,
        1,
        crate::discovery::unix_time(),
        &impersonating,
    )
    .unwrap()
    {
        raw.send_to(&datagram, addr).await.unwrap();
    }

    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        engine.get_peer(&victim.public).is_none(),
        "another key cannot speak for the victim"
    );
    assert!(
        engine.get_peer(&attacker.public).is_none(),
        "an impersonating datagram is dropped entirely"
    );

    // The same sender with a truthful identity is accepted.
    let honest = serde_json::to_vec(&NetworkMessage::SearchQuery {
        query: "hello".to_string(),
        sender: attacker.public,
    })
    .unwrap();
    for datagram in super::envelope::encode_for(
        &attacker,
        &victim_dh.public,
        2,
        crate::discovery::unix_time(),
        &honest,
    )
    .unwrap()
    {
        raw.send_to(&datagram, addr).await.unwrap();
    }
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        engine.get_peer(&attacker.public).is_some(),
        "an honestly signed message registers the sender"
    );

    engine.stop().await;
}

/// A three-relay onion path actually forwards fixed-size cells hop-by-hop and
/// delivers an authenticated request at the exit. Each relay sees only the
/// previous hop's address, and relay work is reported for credit accounting.
#[tokio::test]
async fn test_onion_path_relays_and_delivers_signed_message() {
    fn engine(kp: Keypair) -> NodeEngine {
        NodeEngine::new(NodeConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            keypair: kp,
            bootstrap_peers: Vec::new(),
            stun_server: None,
            max_peers: 100,
            heartbeat_interval: Duration::from_secs(30),
        })
    }

    let kp_origin = Keypair::generate();
    let kp_a = Keypair::generate();
    let kp_b = Keypair::generate();
    let kp_c = Keypair::generate();

    let origin = engine(kp_origin.clone());
    let relay_a = engine(kp_a.clone());
    let relay_b = engine(kp_b.clone());
    let relay_c = engine(kp_c.clone());
    for node in [&origin, &relay_a, &relay_b, &relay_c] {
        node.start().await.expect("node starts");
    }
    let addr_a = relay_a.local_addr().unwrap().unwrap();
    let addr_b = relay_b.local_addr().unwrap().unwrap();
    let addr_c = relay_c.local_addr().unwrap().unwrap();

    // Origin knows the entry hop; each relay knows only its next hop, so the
    // exit can never observe the origin's address.
    origin.connect_to(addr_a, kp_a.public).unwrap();
    relay_a.connect_to(addr_b, kp_b.public).unwrap();
    relay_b.connect_to(addr_c, kp_c.public).unwrap();

    let mut delivered = relay_c.subscribe_onion();
    let mut relayed_a = relay_a.subscribe_relay();
    let mut relayed_b = relay_b.subscribe_relay();

    let path = [
        DhPublicKey::from_ed25519(&kp_a.public).unwrap(),
        DhPublicKey::from_ed25519(&kp_b.public).unwrap(),
        DhPublicKey::from_ed25519(&kp_c.public).unwrap(),
    ];
    origin
        .send_onion_to(addr_a, &path, &NetworkMessage::Ping { nonce: 0x5eed })
        .expect("origin sends onion request");

    let (sender, message) = tokio::time::timeout(Duration::from_secs(2), delivered.recv())
        .await
        .expect("exit delivers")
        .expect("delivery channel open");
    assert_eq!(sender, kp_origin.public);
    assert_eq!(message, NetworkMessage::Ping { nonce: 0x5eed });

    // A message larger than one cell's 118-byte payload must be chunked and
    // reassembled at the exit without changing its authenticated sender.
    let large = NetworkMessage::SearchQuery {
        query: "chunked onion payload ".repeat(30),
        sender: kp_origin.public,
    };
    origin
        .send_onion_to(addr_a, &path, &large)
        .expect("large message sends across cells");
    let (sender, delivered_large) = tokio::time::timeout(Duration::from_secs(2), delivered.recv())
        .await
        .expect("exit reassembles large message")
        .expect("delivery channel open");
    assert_eq!(sender, kp_origin.public);
    assert_eq!(delivered_large, large);

    let event = tokio::time::timeout(Duration::from_secs(2), relayed_a.recv())
        .await
        .expect("entry relay reports work")
        .expect("relay channel open");
    assert_eq!(event.bytes, CELL_SIZE as u64);
    let event_from_b = tokio::time::timeout(Duration::from_secs(2), relayed_b.recv())
        .await
        .expect("middle relay reports work")
        .expect("relay channel open");
    assert_eq!(event_from_b.bytes, CELL_SIZE as u64);

    for node in [&origin, &relay_a, &relay_b, &relay_c] {
        node.stop().await;
    }
}

/// A request sent through an onion circuit gets its answer back along the
/// return path: the exit replies to the second-to-last relay and the reply
/// traverses the earlier relays to reach the origin, which never told the
/// exit its address.
#[tokio::test]
async fn test_onion_request_reply_circuit_round_trips() {
    fn engine(kp: Keypair) -> NodeEngine {
        NodeEngine::new(NodeConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            keypair: kp,
            bootstrap_peers: Vec::new(),
            stun_server: None,
            max_peers: 100,
            heartbeat_interval: Duration::from_secs(30),
        })
    }

    let kp_origin = Keypair::generate();
    let kp_a = Keypair::generate();
    let kp_b = Keypair::generate();
    let kp_c = Keypair::generate();
    let origin = engine(kp_origin.clone());
    let relay_a = engine(kp_a.clone());
    let relay_b = engine(kp_b.clone());
    let relay_c = engine(kp_c.clone());
    for node in [&origin, &relay_a, &relay_b, &relay_c] {
        node.start().await.expect("node starts");
    }
    let addr_a = relay_a.local_addr().unwrap().unwrap();
    let addr_b = relay_b.local_addr().unwrap().unwrap();
    let addr_c = relay_c.local_addr().unwrap().unwrap();
    origin.connect_to(addr_a, kp_a.public).unwrap();
    relay_a.connect_to(addr_b, kp_b.public).unwrap();
    relay_b.connect_to(addr_c, kp_c.public).unwrap();
    // The origin needs a dial address for every relay it names — including the
    // return entry — because it builds the return path before sending.
    origin.connect_to(addr_b, kp_b.public).unwrap();

    let mut requests = relay_c.subscribe_onion_requests();
    let path = [
        DhPublicKey::from_ed25519(&kp_a.public).unwrap(),
        DhPublicKey::from_ed25519(&kp_b.public).unwrap(),
        DhPublicKey::from_ed25519(&kp_c.public).unwrap(),
    ];
    let waiter = {
        let origin = origin.clone();
        let path = path.to_vec();
        tokio::spawn(async move {
            origin
                .send_onion_request(
                    &path,
                    &NetworkMessage::SearchQuery {
                        query: "private query".to_string(),
                        sender: kp_origin.public,
                    },
                    Duration::from_secs(3),
                )
                .await
        })
    };

    let request = tokio::time::timeout(Duration::from_secs(2), requests.recv())
        .await
        .expect("exit delivers the request")
        .expect("request channel open");
    assert_eq!(request.sender, kp_origin.public);
    assert_eq!(
        request.message,
        NetworkMessage::SearchQuery {
            query: "private query".to_string(),
            sender: kp_origin.public,
        }
    );
    assert!(request.can_reply(), "request carries a usable return path");
    let return_path = request.return_path.as_ref().unwrap();
    assert_eq!(
        return_path.first_hop, addr_b,
        "reply enters at the last relay"
    );
    assert_eq!(
        return_path.path.last(),
        Some(&DhPublicKey::from_ed25519(&kp_origin.public).unwrap()),
        "return path ends at the origin's onion key"
    );
    assert!(
        !return_path
            .path
            .contains(&DhPublicKey::from_ed25519(&kp_c.public).unwrap()),
        "the exit does not forward through itself"
    );

    relay_c
        .send_onion_reply(
            &request,
            &NetworkMessage::SearchResponse {
                query: "private query".to_string(),
                results: Vec::new(),
            },
        )
        .expect("exit replies through the return path");

    let reply = tokio::time::timeout(Duration::from_secs(2), waiter)
        .await
        .expect("origin resolves the reply")
        .expect("waiter joins")
        .expect("reply arrives before the circuit times out");
    assert_eq!(
        reply,
        NetworkMessage::SearchResponse {
            query: "private query".to_string(),
            results: Vec::new(),
        }
    );

    for node in [&origin, &relay_a, &relay_b, &relay_c] {
        node.stop().await;
    }
}

/// `FindNode` populates the routing table beyond direct peers: A knows B, B
/// knows C, and a lookup for C's id returns C's advertised contact.
#[tokio::test]
async fn test_find_node_learns_contacts_beyond_direct_peers() {
    fn engine(kp: Keypair) -> NodeEngine {
        NodeEngine::new(NodeConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            keypair: kp,
            bootstrap_peers: Vec::new(),
            stun_server: None,
            max_peers: 100,
            heartbeat_interval: Duration::from_secs(30),
        })
    }

    let kp_a = Keypair::generate();
    let kp_b = Keypair::generate();
    let kp_c = Keypair::generate();
    let a = engine(kp_a.clone());
    let b = engine(kp_b.clone());
    let c = engine(kp_c.clone());
    for node in [&a, &b, &c] {
        node.start().await.unwrap();
    }
    let addr_a = a.local_addr().unwrap().unwrap();
    let addr_b = b.local_addr().unwrap().unwrap();
    let addr_c = c.local_addr().unwrap().unwrap();
    a.connect_to(addr_b, kp_b.public).unwrap();
    b.connect_to(addr_c, kp_c.public).unwrap();

    let target = crate::dht::NodeId::from_public_key(&kp_c.public);
    let contacts = a.find_node(target, Duration::from_millis(500)).await;
    assert!(
        contacts
            .iter()
            .any(|contact| contact.public_key == kp_c.public),
        "lookup learns the peer behind the middle hop"
    );
    assert!(a
        .closest_contacts(&target, 5)
        .iter()
        .any(|contact| { contact.public_key == kp_c.public && contact.address == addr_c }));

    for node in [&a, &b, &c] {
        node.stop().await;
    }
    let _ = addr_a;
}

/// A process restart reuses the same identity but a fresh envelope sequence.
/// The peer's replay window still remembers the previous session's high-water
/// mark, so the new session must start above it or every message is dropped.
#[tokio::test]
async fn test_restarted_peer_is_not_rejected_as_a_replay() {
    fn engine(kp: Keypair) -> NodeEngine {
        NodeEngine::new(NodeConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            keypair: kp,
            bootstrap_peers: Vec::new(),
            stun_server: None,
            max_peers: 100,
            heartbeat_interval: Duration::from_secs(30),
        })
    }

    let kp_a = Keypair::generate();
    let kp_b = Keypair::generate();
    let b = engine(kp_b.clone());
    b.start().await.unwrap();
    let addr_b = b.local_addr().unwrap().unwrap();

    {
        let a = engine(kp_a.clone());
        a.start().await.unwrap();
        a.dial(addr_b, kp_b.public, Duration::from_secs(2))
            .await
            .expect("first session joins");
        for nonce in 0..5 {
            a.send_to(&kp_b.public, NetworkMessage::Ping { nonce })
                .unwrap();
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
        a.stop().await;
    }

    // Same identity, new process, fresh sequence counter.
    let a = engine(kp_a.clone());
    a.start().await.unwrap();
    a.dial(addr_b, kp_b.public, Duration::from_secs(2))
        .await
        .expect("a restarted peer is not mistaken for a replayed old session");

    a.stop().await;
    b.stop().await;
}

/// The lookup is iterative: a target two hops beyond the last directly-known
/// neighbor is only reachable by querying the contacts learned in the first
/// round.
#[tokio::test]
async fn test_find_node_walks_past_the_first_hop() {
    fn engine(kp: Keypair) -> NodeEngine {
        NodeEngine::new(NodeConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            keypair: kp,
            bootstrap_peers: Vec::new(),
            stun_server: None,
            max_peers: 100,
            heartbeat_interval: Duration::from_secs(30),
        })
    }

    let keypairs: Vec<Keypair> = (0..4).map(|_| Keypair::generate()).collect();
    let nodes: Vec<NodeEngine> = keypairs.iter().map(|kp| engine(kp.clone())).collect();
    for node in &nodes {
        node.start().await.unwrap();
    }
    let addrs: Vec<SocketAddr> = nodes
        .iter()
        .map(|node| node.local_addr().unwrap().unwrap())
        .collect();
    // A -> B -> C -> D; each node only dials the next one.
    for index in 0..3 {
        nodes[index]
            .connect_to(addrs[index + 1], keypairs[index + 1].public)
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(100)).await;

    let target = crate::dht::NodeId::from_public_key(&keypairs[3].public);
    let contacts = nodes[0].find_node(target, Duration::from_secs(2)).await;
    assert!(
        contacts.iter().any(|contact| {
            contact.public_key == keypairs[3].public && contact.address == addrs[3]
        }),
        "iterative walk reaches the node three hops away: {contacts:?}"
    );

    for node in &nodes {
        node.stop().await;
    }
}

#[tokio::test]
async fn bootstrap_spec_accepts_literal_addresses_and_dns_names() {
    let keypair = Keypair::generate();
    let spec = format!("127.0.0.1:7921/{}", keypair.public.to_hex());
    let peer = resolve_bootstrap_peer(&spec)
        .await
        .expect("literal bootstrap address resolves");
    assert_eq!(peer.public_key, keypair.public);
    assert_eq!(peer.address, "127.0.0.1:7921".parse().unwrap());

    let spec = format!("localhost:7921/{}", keypair.public.to_hex());
    let peer = resolve_bootstrap_peer(&spec)
        .await
        .expect("DNS bootstrap name resolves");
    assert_eq!(peer.public_key, keypair.public);
    assert_eq!(peer.address.port(), 7921);
}

#[tokio::test]
async fn bootstrap_spec_rejects_malformed_entries() {
    assert!(resolve_bootstrap_peer("seed.example:7921/not-hex")
        .await
        .is_none());
    assert!(resolve_bootstrap_peer("seed.example:7921").await.is_none());
    assert!(resolve_bootstrap_peer("127.0.0.1:7921").await.is_none());
}

/// Cover traffic is off by default and, when enabled with a fast schedule,
/// actually emits fixed-size decoy cells toward live peers.
#[tokio::test]
async fn cover_traffic_is_opt_in_and_emits_decoy_cells() {
    fn engine(keypair: Keypair) -> NodeEngine {
        NodeEngine::new(NodeConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            keypair,
            bootstrap_peers: Vec::new(),
            stun_server: None,
            max_peers: 32,
            heartbeat_interval: Duration::from_secs(30),
        })
    }

    let origin_key = Keypair::generate();
    let relays: Vec<Keypair> = (0..3).map(|_| Keypair::generate()).collect();
    let origin = engine(origin_key.clone());
    let relay_engines: Vec<NodeEngine> = relays.iter().map(|kp| engine(kp.clone())).collect();

    // Disabled by default: no configuration and no emitted cells.
    assert!(origin.cover_traffic().is_none());

    origin.enable_cover_traffic(CoverConfig {
        mean_interval: Duration::from_millis(50),
        min_interval: Duration::from_millis(40),
        max_interval: Duration::from_millis(80),
    });
    assert!(origin.cover_traffic().is_some());

    for node in std::iter::once(&origin).chain(relay_engines.iter()) {
        node.start().await.expect("engine starts");
    }
    for (index, relay) in relay_engines.iter().enumerate() {
        let addr = relay.local_addr().unwrap().unwrap();
        origin.connect_to(addr, relays[index].public).unwrap();
    }

    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if origin.metrics().snapshot().onion_cover_cells_sent > 0 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "cover traffic did not emit a decoy cell"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    origin.stop().await;
    for node in &relay_engines {
        node.stop().await;
    }
}

/// `SendFlags::E2EE` seals the payload for its recipient: the receiving
/// engine opens it before dispatch, so the peer's subscribers get the inner
/// message (never the `Sealed` wrapper), and the opened counter ticks. Any
/// message type can opt in — the flag is transport-level, not chat-specific.
#[tokio::test]
async fn test_e2ee_flag_delivers_the_inner_message_to_subscribers() {
    fn engine(kp: Keypair) -> NodeEngine {
        NodeEngine::new(NodeConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            keypair: kp,
            bootstrap_peers: Vec::new(),
            stun_server: None,
            max_peers: 100,
            heartbeat_interval: Duration::from_secs(30),
        })
    }

    let kp_a = Keypair::generate();
    let kp_b = Keypair::generate();
    let a = engine(kp_a.clone());
    let b = engine(kp_b.clone());
    a.start().await.unwrap();
    b.start().await.unwrap();
    let addr_b = b.local_addr().unwrap().unwrap();

    let mut rx_b = b.subscribe();
    a.dial(addr_b, kp_b.public, Duration::from_secs(2))
        .await
        .expect("A joins B's table");

    let chat = NetworkMessage::Chat {
        sender: kp_a.public,
        id: [1u8; 16],
        sent_at: 1_700_000_000,
        text: "sealed hello".to_string(),
    };
    let query = NetworkMessage::SearchQuery {
        query: "sealed query".to_string(),
        sender: kp_a.public,
    };
    a.send_to_with(&kp_b.public, chat.clone(), SendFlags::E2EE)
        .expect("sealed chat sends");
    a.send_to_with(&kp_b.public, query.clone(), SendFlags::E2EE)
        .expect("sealed query sends");

    let seen = tokio::time::timeout(Duration::from_secs(3), async {
        let mut seen = Vec::new();
        while !(seen.contains(&chat) && seen.contains(&query)) {
            match rx_b.recv().await {
                Some((_, message)) => seen.push(message),
                None => break,
            }
        }
        seen
    })
    .await
    .expect("both sealed messages arrive");

    assert!(
        seen.contains(&chat),
        "the inner chat message arrives unsealed: {seen:?}"
    );
    assert!(
        seen.contains(&query),
        "a non-chat message round-trips through the same flag: {seen:?}"
    );
    assert!(
        !seen
            .iter()
            .any(|m| matches!(m, NetworkMessage::Sealed { .. })),
        "subscribers never handle the Sealed wrapper"
    );
    assert!(
        b.metrics().snapshot().e2ee_messages_opened >= 2,
        "the opened counter tracks sealed deliveries"
    );

    a.stop().await;
    b.stop().await;
}

/// A sealed payload whose inner identity disagrees with the envelope signer
/// is dropped: sealing cannot launder a forged sender, and the drop is
/// counted.
#[tokio::test]
async fn test_e2ee_rejects_a_forged_inner_sender() {
    fn engine(kp: Keypair) -> NodeEngine {
        NodeEngine::new(NodeConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            keypair: kp,
            bootstrap_peers: Vec::new(),
            stun_server: None,
            max_peers: 100,
            heartbeat_interval: Duration::from_secs(30),
        })
    }

    let kp_a = Keypair::generate();
    let kp_b = Keypair::generate();
    let stranger = Keypair::generate();
    let a = engine(kp_a.clone());
    let b = engine(kp_b.clone());
    a.start().await.unwrap();
    b.start().await.unwrap();
    let addr_b = b.local_addr().unwrap().unwrap();

    let mut rx_b = b.subscribe();
    a.dial(addr_b, kp_b.public, Duration::from_secs(2))
        .await
        .expect("A joins B's table");

    // A seals a chat that claims to come from someone else: the crypto opens
    // fine, but the inner sender must equal the envelope signer.
    let forged = NetworkMessage::Chat {
        sender: stranger.public,
        id: [2u8; 16],
        sent_at: 0,
        text: "not from A".to_string(),
    };
    let sealed = a
        .seal_message(&kp_b.public, forged)
        .expect("A seals its own message");
    a.send_to(&kp_b.public, sealed)
        .expect("the pre-sealed wrapper sends");

    let delivered = tokio::time::timeout(Duration::from_millis(400), async {
        loop {
            match rx_b.recv().await {
                Some((_, message @ NetworkMessage::Chat { .. })) => {
                    return Some(message);
                }
                Some(_) => continue,
                None => return None,
            }
        }
    })
    .await;
    assert!(
        delivered.is_err(),
        "a sealed message with a forged inner sender must never be delivered"
    );
    assert!(
        b.metrics().snapshot().e2ee_messages_dropped >= 1,
        "the drop is counted"
    );

    a.stop().await;
    b.stop().await;
}
