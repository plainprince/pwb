//! Integration tests for PlainWeb network join: two live nodes on loopback
//! UDP can discover each other via connection links and exchange
//! `NetworkMessage` datagrams end-to-end.

use pwb_core::crypto::{ContentId, Keypair};
use pwb_core::net::{NetworkMessage, NodeConfig, NodeEngine};
use std::net::SocketAddr;
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
        .broadcast(NetworkMessage::gossip_announce(&kp_a, cid, "new artifact"))
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

/// Full `chat.plain` stack: two live nodes pair over loopback UDP, the
/// sending `ChatService` records its outbound echo, the receiving service's
/// loop stores the inbound message, and the transport opened the E2EE-sealed
/// payload on the way (subscribers never see the wrapper).
#[tokio::test]
async fn test_chat_service_round_trip_over_e2ee() {
    let kp_a = Keypair::generate();
    let (engine_a, addr_a) = start_engine(kp_a.clone()).await;
    let kp_b = Keypair::generate();
    let (engine_b, addr_b) = start_engine(kp_b.clone()).await;

    // Pair both directions so either side can dial the other directly.
    engine_a
        .launch_connection(&NodeEngine::build_connection_link(addr_b, kp_b.public))
        .expect("A connects to B");
    engine_b
        .launch_connection(&NodeEngine::build_connection_link(addr_a, kp_a.public))
        .expect("B connects to A");

    // B's receive loop records inbound chats into its per-peer transcript
    // and answers each one with a delivery receipt; A runs the same loop so
    // B's receipt has somewhere to land.
    let chat_b = pwb_node::chat::ChatService::new(engine_b.clone());
    tokio::spawn(std::sync::Arc::clone(&chat_b).run());
    let chat_a = pwb_node::chat::ChatService::new(engine_a.clone());
    tokio::spawn(std::sync::Arc::clone(&chat_a).run());
    // Let both tasks subscribe before the message lands.
    tokio::time::sleep(Duration::from_millis(100)).await;

    chat_a
        .send(&kp_b.public, "hello over e2ee")
        .expect("the sealed chat sends");
    tokio::time::sleep(Duration::from_millis(300)).await;

    let echo = chat_a.snapshot().transcript(&kp_b.public).to_vec();
    assert_eq!(echo.len(), 1, "A keeps its outbound echo");
    assert!(echo[0].outbound);
    assert_eq!(echo[0].text, "hello over e2ee");

    let inbox = chat_b.snapshot().transcript(&kp_a.public).to_vec();
    assert_eq!(inbox.len(), 1, "B received the chat");
    assert!(!inbox[0].outbound);
    assert_eq!(inbox[0].from, kp_a.public);
    assert_eq!(inbox[0].text, "hello over e2ee");
    assert!(
        engine_b.metrics().snapshot().e2ee_messages_opened >= 1,
        "the payload travelled sealed and was opened on receipt"
    );

    // A successful send is not delivery: the entry settles only when B's
    // receipt for that exact message id arrives back over the transport.
    wait_for_receipt(&chat_a, &kp_b.public, "B confirmed the chat").await;

    engine_a.stop().await;
    engine_b.stop().await;
}

/// Poll one transcript until the peer's receipt settles its last outbound
/// entry (or fail the test with the missing half named).
async fn wait_for_receipt(
    chat: &std::sync::Arc<pwb_node::chat::ChatService>,
    peer: &pwb_core::crypto::PublicKey,
    what: &str,
) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let transcript = chat.snapshot().transcript(peer).to_vec();
        if transcript.last().and_then(|entry| entry.ack) == Some(true) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{what}: the last outbound entry stayed at {:?}",
            transcript.last().map(|entry| entry.ack)
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Onion-circuit chat: A talks to D across a three-hop circuit relayed by B
/// and C, with the payload sealed end-to-end before it enters the circuit.
/// Both endpoints record the conversation with the onion marker, the exit
/// opened the E2EE wrapper (never the relays), and D never needed a direct
/// exchange with A beyond being in each other's peer tables.
#[tokio::test]
async fn test_chat_over_an_onion_circuit() {
    let kp_a = Keypair::generate();
    let (engine_a, addr_a) = start_engine(kp_a.clone()).await;
    let kp_b = Keypair::generate();
    let (engine_b, addr_b) = start_engine(kp_b.clone()).await;
    let kp_c = Keypair::generate();
    let (engine_c, addr_c) = start_engine(kp_c.clone()).await;
    let kp_d = Keypair::generate();
    let (engine_d, addr_d) = start_engine(kp_d.clone()).await;

    // A fully mesh the four nodes: A picks its entry/middle hops randomly,
    // and every relay must be able to address the next hop, so all directed
    // pairs get registered.
    let engines = [&engine_a, &engine_b, &engine_c, &engine_d];
    let keys = [&kp_a.public, &kp_b.public, &kp_c.public, &kp_d.public];
    let addrs = [addr_a, addr_b, addr_c, addr_d];
    for (i, engine) in engines.iter().enumerate() {
        for (j, addr) in addrs.iter().enumerate() {
            if i == j {
                continue;
            }
            engine
                .launch_connection(&NodeEngine::build_connection_link(*addr, *keys[j]))
                .expect("mesh link registers");
        }
    }

    // Both endpoints run the chat receive loop (direct + onion subscriptions).
    let chat_a = pwb_node::chat::ChatService::new(engine_a.clone());
    let chat_d = pwb_node::chat::ChatService::new(engine_d.clone());
    tokio::spawn(std::sync::Arc::clone(&chat_a).run());
    tokio::spawn(std::sync::Arc::clone(&chat_d).run());
    // Let both tasks subscribe before the message lands.
    tokio::time::sleep(Duration::from_millis(100)).await;

    chat_a
        .send_via(
            &kp_d.public,
            "hello over a circuit",
            pwb_node::chat::ChatRoute::Onion,
        )
        .expect("the onion chat sends (three direct peers exist for the circuit)");
    tokio::time::sleep(Duration::from_millis(500)).await;

    let echo = chat_a.snapshot().transcript(&kp_d.public).to_vec();
    assert_eq!(echo.len(), 1, "A keeps its outbound echo");
    assert!(echo[0].outbound);
    assert!(echo[0].via_onion, "the send took the circuit");
    assert_eq!(echo[0].text, "hello over a circuit");

    let inbox = chat_d.snapshot().transcript(&kp_a.public).to_vec();
    assert_eq!(inbox.len(), 1, "D received the chat through the exit");
    assert!(!inbox[0].outbound);
    assert!(inbox[0].via_onion, "D saw it arrive over the circuit");
    assert_eq!(inbox[0].from, kp_a.public);
    assert_eq!(inbox[0].text, "hello over a circuit");
    assert!(
        engine_d.metrics().snapshot().e2ee_messages_opened >= 1,
        "the exit opened the E2EE wrapper on D's behalf"
    );

    // D's receipt travelled back down the circuit's return path: A never
    // gave D its address to reply to, so this only works over the circuit.
    wait_for_receipt(&chat_a, &kp_d.public, "D confirmed the circuit chat").await;

    engine_a.stop().await;
    engine_b.stop().await;
    engine_c.stop().await;
    engine_d.stop().await;
}

/// Wait until `condition` holds, or fail the test with what was missing.
async fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A public room spans the whole network, not just who you are talking to:
/// A posts into `lobby` over a chain of peers (A—B—C—D), the message is
/// relayed by C **even though C never joined**, and D — three hops away —
/// ends up with exactly one copy. The one-hop roster learns who announced,
/// and a member that joins after the fact catches up from a peer's history.
#[tokio::test]
async fn test_public_room_floods_across_the_network_and_late_joiners_sync() {
    let kp_a = Keypair::generate();
    let (engine_a, addr_a) = start_engine(kp_a.clone()).await;
    let kp_b = Keypair::generate();
    let (engine_b, addr_b) = start_engine(kp_b.clone()).await;
    let kp_c = Keypair::generate();
    let (engine_c, addr_c) = start_engine(kp_c.clone()).await;
    let kp_d = Keypair::generate();
    let (engine_d, addr_d) = start_engine(kp_d.clone()).await;

    // A chain: each node knows only its neighbours, so a post cannot reach
    // the far end without being forwarded by the middle.
    let links = [
        (&engine_a, addr_b, &kp_b.public),
        (&engine_b, addr_a, &kp_a.public),
        (&engine_b, addr_c, &kp_c.public),
        (&engine_c, addr_b, &kp_b.public),
        (&engine_c, addr_d, &kp_d.public),
        (&engine_d, addr_c, &kp_c.public),
    ];
    for (engine, addr, key) in links {
        engine
            .launch_connection(&NodeEngine::build_connection_link(addr, *key))
            .expect("chain link registers");
    }

    let room_a = pwb_node::room::RoomService::new(engine_a.clone(), kp_a.clone());
    let room_b = pwb_node::room::RoomService::new(engine_b.clone(), kp_b.clone());
    // C runs the service (so it relays) but never joins: a bystander still
    // carries other people's rooms.
    let room_c = pwb_node::room::RoomService::new(engine_c.clone(), kp_c.clone());
    let room_d = pwb_node::room::RoomService::new(engine_d.clone(), kp_d.clone());
    tokio::spawn(std::sync::Arc::clone(&room_a).run());
    tokio::spawn(std::sync::Arc::clone(&room_b).run());
    tokio::spawn(std::sync::Arc::clone(&room_c).run());
    tokio::spawn(std::sync::Arc::clone(&room_d).run());
    // Let every task subscribe before anything is announced.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Join the listeners before the announcer, so A's membership cannot
    // arrive at a peer that has not opened the room yet (rosters only
    // track rooms the node is itself in).
    room_b.join(pwb_node::room::DEFAULT_ROOM).expect("B joins");
    room_d.join(pwb_node::room::DEFAULT_ROOM).expect("D joins");
    room_a.join(pwb_node::room::DEFAULT_ROOM).expect("A joins");

    // Subscriptions are one hop: A's announcement reaches its direct peer B,
    // and B answers with its own, so both rosters learn about the other.
    wait_until("B to learn that A is here", || {
        room_b
            .snapshot()
            .rooms
            .get(pwb_node::room::DEFAULT_ROOM)
            .is_some_and(|state| state.subscribers.contains(&kp_a.public))
    })
    .await;
    wait_until("A to learn that B is here", || {
        room_a
            .snapshot()
            .rooms
            .get(pwb_node::room::DEFAULT_ROOM)
            .is_some_and(|state| state.subscribers.contains(&kp_b.public))
    })
    .await;

    room_a
        .post(pwb_node::room::DEFAULT_ROOM, "hello from A")
        .expect("A posts");

    // D is three hops from A and only ever hears about it by relaying.
    wait_until("D to see A's post through two relays", || {
        room_d
            .snapshot()
            .rooms
            .get(pwb_node::room::DEFAULT_ROOM)
            .is_some_and(|state| state.posts.iter().any(|post| post.text == "hello from A"))
    })
    .await;
    let state_d = room_d.snapshot();
    let copies = state_d.rooms[pwb_node::room::DEFAULT_ROOM]
        .posts
        .iter()
        .filter(|post| post.text == "hello from A")
        .count();
    assert_eq!(copies, 1, "flooding delivers one copy, not a storm");
    let post_d = &state_d.rooms[pwb_node::room::DEFAULT_ROOM].posts[0];
    assert_eq!(post_d.sender, kp_a.public, "authorship survives the relays");
    assert!(
        post_d.verify(),
        "the author's signature verifies at the far end"
    );

    // C relayed it without ever joining: a public room arrives through
    // bystanders, but they keep nothing.
    assert!(
        room_c
            .snapshot()
            .rooms
            .get(pwb_node::room::DEFAULT_ROOM)
            .is_none_or(|state| state.posts.is_empty()),
        "a non-member relays without storing"
    );

    // A late joiner asks for history instead of waiting for the next post.
    room_c
        .join(pwb_node::room::DEFAULT_ROOM)
        .expect("C joins later");
    wait_until("C to catch up from a peer's history", || {
        room_c
            .snapshot()
            .rooms
            .get(pwb_node::room::DEFAULT_ROOM)
            .is_some_and(|state| state.posts.iter().any(|post| post.text == "hello from A"))
    })
    .await;
    let state_c = room_c.snapshot();
    let copies = state_c.rooms[pwb_node::room::DEFAULT_ROOM]
        .posts
        .iter()
        .filter(|post| post.text == "hello from A")
        .count();
    assert_eq!(copies, 1, "history and the live copy are the same message");

    engine_a.stop().await;
    engine_b.stop().await;
    engine_c.stop().await;
    engine_d.stop().await;
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
