use super::*;
use crate::crypto::{ContentId, DhPublicKey, Keypair, Signature};
use crate::onion::{OnionCell, OnionError, CELL_SIZE};
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
    let kp = Keypair::generate();
    let config = NodeConfig {
        listen_addr: SocketAddr::from_str("127.0.0.1:0").unwrap(),
        keypair: kp.clone(),
        bootstrap_peers: Vec::new(),
        max_peers: 100,
        heartbeat_interval: Duration::from_secs(30),
    };

    let engine = NodeEngine::new(config);
    let _ = engine.start().await;

    // Create a peer discovery message from our own keypair
    let message = NetworkMessage::PeerDiscovery {
        known_peers: Vec::new(),
        sender_pubkey: kp.public,
    };

    // Note: The send_to method requires the peer to be in the peer table first
    // So we'll use the broadcast method instead for testing message processing
    // Since both send_to and broadcast use the same message channel, this tests
    // that messages are properly processed through the handler loop
    let send_result = engine.broadcast(message);
    assert!(send_result.is_ok());

    // Give the message handler a moment to process the message
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Our own peer is automatically added to the peer table when the engine starts
    let peer = engine.get_peer(&kp.public);
    assert!(peer.is_some());
    assert_eq!(peer.unwrap().public_key, kp.public);

    engine.stop().await;
}

#[tokio::test]
async fn test_message_processing_with_known_peers() {
    let kp = Keypair::generate();
    let config = NodeConfig {
        listen_addr: SocketAddr::from_str("127.0.0.1:0").unwrap(),
        keypair: kp.clone(),
        bootstrap_peers: Vec::new(),
        max_peers: 100,
        heartbeat_interval: Duration::from_secs(30),
    };

    let engine = NodeEngine::new(config);
    let _ = engine.start().await;

    // Create two additional peer keypairs
    let peer1_kp = Keypair::generate();
    let peer2_kp = Keypair::generate();

    // Create a peer discovery message that includes known peers
    let message = NetworkMessage::PeerDiscovery {
        known_peers: vec![peer1_kp.public, peer2_kp.public],
        sender_pubkey: kp.public,
    };

    // Broadcast the message to test processing
    let send_result = engine.broadcast(message);
    assert!(send_result.is_ok());

    // Give the message handler a moment to process the message
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Now all three peers should be in the peer table
    let peer1 = engine.get_peer(&peer1_kp.public);
    let peer2 = engine.get_peer(&peer2_kp.public);
    let self_peer = engine.get_peer(&kp.public);

    assert!(peer1.is_some());
    assert!(peer2.is_some());
    assert!(self_peer.is_some());

    // Check that they have the correct public keys and default reputation
    let peer1_info = peer1.unwrap();
    let peer2_info = peer2.unwrap();
    let self_peer_info = self_peer.unwrap();
    assert_eq!(peer1_info.public_key, peer1_kp.public);
    assert_eq!(peer1_info.reputation, 0);
    assert_eq!(peer2_info.public_key, peer2_kp.public);
    assert_eq!(peer2_info.reputation, 0);
    assert_eq!(self_peer_info.public_key, kp.public);
    assert_eq!(self_peer_info.reputation, 0);

    engine.stop().await;
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
/// channel, and gossip announcements register/refresh the publisher.
#[tokio::test]
async fn test_node_engine_error_paths_and_gossip_registration() {
    let config = NodeConfig {
        listen_addr: SocketAddr::from_str("127.0.0.1:0").unwrap(),
        keypair: Keypair::generate(),
        bootstrap_peers: Vec::new(),
        max_peers: 100,
        heartbeat_interval: Duration::from_secs(30),
    };
    let engine = NodeEngine::new(config);

    engine.start().await.expect("first start");
    engine.start().await.expect("second start is a no-op");
    assert!(
        engine.peer_table().lock().unwrap().is_empty(),
        "no peers registered before any traffic"
    );

    // Unknown peer → PeerNotFound before anything is queued.
    let stranger = Keypair::generate().public;
    let err = engine
        .send_to(
            &stranger,
            NetworkMessage::SearchQuery {
                query: "x".to_string(),
                sender: Keypair::generate().public,
            },
        )
        .unwrap_err();
    assert!(matches!(err, NetworkError::PeerNotFound(_)));

    // A gossip announcement registers the publisher and refreshes it.
    let publisher = Keypair::generate().public;
    engine
        .broadcast(NetworkMessage::GossipAnnounce {
            content_id: ContentId([1u8; 32]),
            title: "artifact".to_string(),
            publisher,
            signature: Signature([2u8; 64]),
        })
        .expect("broadcast enqueues");
    tokio::time::sleep(Duration::from_millis(50)).await;
    let seen = engine.get_peer(&publisher).expect("publisher registered");
    assert_eq!(seen.public_key, publisher);

    // Second announcement refreshes last_seen on the existing entry.
    engine
        .broadcast(NetworkMessage::GossipAnnounce {
            content_id: ContentId([3u8; 32]),
            title: "artifact 2".to_string(),
            publisher,
            signature: Signature([4u8; 64]),
        })
        .expect("broadcast enqueues");
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(engine.get_peer(&publisher).is_some());

    engine.stop().await;
}

#[tokio::test]
async fn test_launch_connection_valid_link() {
    let kp = Keypair::generate();
    let config = NodeConfig {
        listen_addr: SocketAddr::from_str("127.0.0.1:0").unwrap(),
        keypair: kp.clone(),
        bootstrap_peers: Vec::new(),
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
