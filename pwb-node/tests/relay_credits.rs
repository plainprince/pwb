//! Relay credits: a node that forwards onion cells mints against the bytes it
//! actually relayed, into its signed credit ledger.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use pwb_core::crypto::{DhKeypair, DhPublicKey, Keypair};
use pwb_core::net::{NetworkMessage, NodeConfig, NodeEngine};
use pwb_core::onion::OnionCell;
use pwb_node::credit::CreditLedger;
use pwb_node::relay::RelayCreditService;

fn engine(keypair: Keypair) -> NodeEngine {
    NodeEngine::new(NodeConfig {
        listen_addr: "127.0.0.1:0".parse().unwrap(),
        keypair,
        bootstrap_peers: Vec::new(),
        stun_server: None,
        max_peers: 100,
        heartbeat_interval: Duration::from_secs(30),
    })
}

#[tokio::test]
async fn relayed_onion_cells_mint_into_the_signed_ledger() {
    let kp_origin = Keypair::generate();
    let kp_a = Keypair::generate();
    let kp_b = Keypair::generate();
    let kp_c = Keypair::generate();
    let origin = engine(kp_origin);
    let a = engine(kp_a.clone());
    let b = engine(kp_b.clone());
    let c = engine(kp_c.clone());
    for node in [&origin, &a, &b, &c] {
        node.start().await.unwrap();
    }
    let addr_a = a.local_addr().unwrap().unwrap();
    let addr_b = b.local_addr().unwrap().unwrap();
    let addr_c = c.local_addr().unwrap().unwrap();
    origin.connect_to(addr_a, kp_a.public).unwrap();
    a.connect_to(addr_b, kp_b.public).unwrap();
    b.connect_to(addr_c, kp_c.public).unwrap();

    let ledger = Arc::new(CreditLedger::new(kp_a.clone(), None, 0));
    let relay = Arc::new(RelayCreditService::with_defaults(
        a.clone(),
        &kp_a,
        Arc::clone(&ledger),
    ));
    let handle = tokio::spawn(Arc::clone(&relay).run(kp_a.clone()));

    let path = [
        DhPublicKey::from_ed25519(&kp_a.public).unwrap(),
        DhPublicKey::from_ed25519(&kp_b.public).unwrap(),
        DhPublicKey::from_ed25519(&kp_c.public).unwrap(),
    ];
    // Two cells fill the first whole-KiB relay window.
    for nonce in 0..2u64 {
        origin
            .send_onion_to(addr_a, &path, &NetworkMessage::Ping { nonce })
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(ledger.balance(), 1, "1024 relayed bytes mint one credit");

    // The per-service cooldown holds the next window until real time passes.
    for nonce in 2..4u64 {
        origin
            .send_onion_to(addr_a, &path, &NetworkMessage::Ping { nonce })
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(1200)).await;
    for nonce in 4..6u64 {
        origin
            .send_onion_to(addr_a, &path, &NetworkMessage::Ping { nonce })
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        ledger.balance() >= 2,
        "later windows mint after the cooldown, got {}",
        ledger.balance()
    );
    assert!(ledger.entries().iter().any(|entry| entry.kind == "relay"));

    handle.abort();
    for node in [&origin, &a, &b, &c] {
        node.stop().await;
    }
    let _: SocketAddr = addr_a;
}

/// A peer that seals cells to a relay's *public* onion key and names an
/// unregistered next hop must not mint relay credit: the node peeled the cell
/// but never forwarded it, so there is no relayed service to pay for.
#[tokio::test]
async fn cells_peeled_but_not_forwarded_mint_nothing() {
    let kp_a = Keypair::generate();
    let a = engine(kp_a.clone());
    a.start().await.unwrap();
    let addr_a = a.local_addr().unwrap().unwrap();

    let ledger = Arc::new(CreditLedger::new(kp_a.clone(), None, 0));
    let relay = Arc::new(RelayCreditService::with_defaults(
        a.clone(),
        &kp_a,
        Arc::clone(&ledger),
    ));
    let handle = tokio::spawn(Arc::clone(&relay).run(kp_a.clone()));

    // Front hop is this node; the remaining hops are keys nobody peers with,
    // so `resolve_onion_address` has no address to forward to.
    let target = DhPublicKey::from_ed25519(&kp_a.public).unwrap();
    let unknown_b = DhKeypair::generate().public;
    let unknown_c = DhKeypair::generate().public;
    let path = [target, unknown_b, unknown_c];

    let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    // Four cells is two whole-KiB windows: enough to mint under the old
    // emission point, where the event fired straight after peeling.
    for nonce in 0..4u64 {
        let payload = serde_json::to_vec(&NetworkMessage::Ping { nonce }).unwrap();
        let cell = OnionCell::encapsulate(&payload, &path).unwrap();
        socket.send_to(&cell.to_bytes(), addr_a).await.unwrap();
    }
    tokio::time::sleep(Duration::from_millis(250)).await;

    assert_eq!(
        ledger.balance(),
        0,
        "peeling a cell that is never forwarded must not mint"
    );
    assert!(!ledger.entries().iter().any(|entry| entry.kind == "relay"));

    handle.abort();
    a.stop().await;
}
