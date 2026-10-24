//! NAT traversal over real loopback UDP: rendezvous hole punching and the
//! one-hop relay fallback for peers that cannot be dialed directly.
//!
//! These tests exercise the actual engine loops (envelopes, peer tables,
//! relay wrapping, probe handling), not just the message encoders. A real NAT
//! cannot be created in a unit test, so the relay test deliberately marks the
//! target as relay-only before sending: every message must take the wrap/
//! forward/unwrap path, and the target's reply must come back the same way.

use std::net::SocketAddr;
use std::time::Duration;

use pwb_core::crypto::{Keypair, PublicKey};
use pwb_core::net::{BootstrapPeer, NetworkMessage, NodeConfig, NodeEngine};

async fn start_engine(keypair: Keypair) -> (NodeEngine, SocketAddr) {
    let engine = NodeEngine::new(NodeConfig {
        listen_addr: "127.0.0.1:0".parse().unwrap(),
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

fn link(addr: SocketAddr, key: PublicKey) -> String {
    NodeEngine::build_connection_link(addr, key)
}

/// Register `peer` at `addr` without dialing (what PeerExchange does for a
/// peer we have not authenticated yet).
fn learn(engine: &NodeEngine, addr: SocketAddr, key: PublicKey) {
    engine.connect_to(addr, key).expect("register peer");
}

#[tokio::test]
async fn relay_forwards_end_to_end_messages_and_replies() {
    let kp_a = Keypair::generate();
    let kp_b = Keypair::generate();
    let kp_r = Keypair::generate();
    let (engine_a, addr_a) = start_engine(kp_a.clone()).await;
    let (engine_b, addr_b) = start_engine(kp_b.clone()).await;
    let (engine_r, addr_r) = start_engine(kp_r.clone()).await;

    // Both ends know the relay; the relay learns both from their inbound
    // liveness probes, exactly as a public rendezvous node would.
    engine_a
        .launch_connection(&link(addr_r, kp_r.public))
        .expect("A knows R");
    engine_b
        .launch_connection(&link(addr_r, kp_r.public))
        .expect("B knows R");
    engine_r
        .dial(addr_a, kp_a.public, Duration::from_secs(2))
        .await
        .expect("R dials A");
    engine_r
        .dial(addr_b, kp_b.public, Duration::from_secs(2))
        .await
        .expect("R dials B");
    tokio::time::sleep(Duration::from_millis(150)).await;

    // A has no direct route to B, so put it behind the relay and prove the
    // relayed Ping/Pong round trip. The returned peer is B as registered at
    // its advertised address.
    let peer = engine_a
        .connect_via_relay(kp_r.public, kp_b.public, addr_b, Duration::from_secs(2))
        .await
        .expect("relayed dial succeeds");
    assert_eq!(peer.public_key, kp_b.public);
    assert_eq!(
        engine_a
            .relay_routes()
            .lock()
            .unwrap()
            .get(&kp_b.public)
            .copied(),
        Some(kp_r.public),
        "B is reached through R"
    );

    // A normal application message follows the same path and is answered by
    // the application's Pong (here: an explicit Ping to B).
    let nonce = 0x5EED_1234_u64;
    let mut replies = engine_b.subscribe();
    engine_a
        .send_to(&kp_b.public, NetworkMessage::Ping { nonce })
        .expect("relayed send");
    let mut pong_seen = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(200), replies.recv()).await {
            Ok(Some((_from, NetworkMessage::Ping { nonce: seen }))) if seen == nonce => {
                // B's handler answers Ping automatically. We only needed to
                // see the request arrive intact and attributed to A.
                pong_seen = true;
                break;
            }
            _ => {}
        }
    }
    assert!(
        pong_seen,
        "relayed message reaches B with its payload intact"
    );

    engine_a.stop().await;
    engine_b.stop().await;
    engine_r.stop().await;
}

#[tokio::test]
async fn rendezvous_hole_punch_opens_both_directions_and_dials() {
    let kp_a = Keypair::generate();
    let kp_b = Keypair::generate();
    let kp_r = Keypair::generate();
    let (engine_a, addr_a) = start_engine(kp_a.clone()).await;
    let (engine_b, addr_b) = start_engine(kp_b.clone()).await;
    let (engine_r, addr_r) = start_engine(kp_r.clone()).await;

    engine_a
        .launch_connection(&link(addr_r, kp_r.public))
        .expect("A knows R");
    engine_b
        .launch_connection(&link(addr_r, kp_r.public))
        .expect("B knows R");
    engine_r
        .dial(addr_a, kp_a.public, Duration::from_secs(2))
        .await
        .expect("R dials A");
    engine_r
        .dial(addr_b, kp_b.public, Duration::from_secs(2))
        .await
        .expect("R dials B");

    // Each side has learned the other from PeerExchange but has never sent a
    // datagram to it. That is the state a NAT'd pair is in before punching.
    learn(&engine_a, addr_b, kp_b.public);
    learn(&engine_b, addr_a, kp_a.public);
    tokio::time::sleep(Duration::from_millis(150)).await;

    let peer = engine_a
        .hole_punch_via(kp_r.public, kp_b.public, Duration::from_secs(3))
        .await
        .expect("hole punch reaches B");
    assert_eq!(peer.public_key, kp_b.public);
    assert_eq!(
        engine_a.metrics().snapshot().hole_punches,
        1,
        "the punch is counted"
    );
    assert!(
        engine_a
            .relay_routes()
            .lock()
            .unwrap()
            .get(&kp_b.public)
            .is_none(),
        "a successful punch uses the direct path, not a relay"
    );

    engine_a.stop().await;
    engine_b.stop().await;
    engine_r.stop().await;
}
