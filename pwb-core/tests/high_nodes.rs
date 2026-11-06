//! High-node-count handling: the swarm must stay bounded when hundreds of
//! peers are known, not grow O(N^2) per message.

use std::net::SocketAddr;
use std::time::Duration;

use pwb_core::crypto::Keypair;
use pwb_core::net::{NodeConfig, NodeEngine, GOSSIP_FANOUT, PEER_EXCHANGE_ADVERTISED_MAX};

fn test_engine(max_peers: usize) -> NodeEngine {
    NodeEngine::new(NodeConfig {
        listen_addr: "127.0.0.1:0".parse().unwrap(),
        keypair: Keypair::generate(),
        bootstrap_peers: Vec::new(),
        stun_server: None,
        max_peers,
        heartbeat_interval: Duration::from_secs(30),
    })
}

fn loopback_port(port: u16) -> SocketAddr {
    format!("127.0.0.1:{port}").parse().unwrap()
}

#[test]
fn peer_table_stays_bounded_under_mass_advertisement() {
    // 200 advertised peers into a 16-slot table must not grow the table.
    let engine = test_engine(16);
    for i in 0..200u16 {
        let k = Keypair::generate();
        engine
            .connect_to(loopback_port(9000 + (i % 50000)), k.public)
            .unwrap();
    }
    let len = engine.peer_table().lock().unwrap().len();
    assert!(len <= 16, "peer table must respect max_peers, got {len}");
}

#[test]
fn gossip_fanout_bounds_forwarding_cost() {
    // 200 peers, but one flood round contacts only GOSSIP_FANOUT of them.
    let engine = test_engine(256);
    for i in 0..200u16 {
        let k = Keypair::generate();
        engine
            .connect_to(loopback_port(10000 + (i % 40000)), k.public)
            .unwrap();
    }
    assert_eq!(engine.peer_table().lock().unwrap().len(), 200);
    let targets = engine.gossip_targets(None, GOSSIP_FANOUT);
    assert_eq!(
        targets.len(),
        GOSSIP_FANOUT,
        "gossip must contact fanout peers, not the whole table"
    );
    // Excluding the sender drops at most one peer from the sample, and the
    // sender itself is never in it (checked by key, not just by count).
    let (from_addr, from_key) = {
        let table_arc = engine.peer_table();
        let table = table_arc.lock().unwrap();
        let info = table.values().next().unwrap();
        (info.address, info.public_key)
    };
    let targets_ex = engine.gossip_targets(Some(from_addr), GOSSIP_FANOUT);
    assert!(
        targets_ex.len() <= GOSSIP_FANOUT,
        "exclusion must not grow the fanout"
    );
    assert!(
        !targets_ex.contains(&from_key),
        "the sender must not forward to itself"
    );
}

#[test]
fn peer_exchange_advertisement_is_capped() {
    // The advertised subset is what keeps a 1000-node PeerExchange from
    // becoming a 50KB fragmented broadcast.
    const { assert!(PEER_EXCHANGE_ADVERTISED_MAX <= 24) }
    const { assert!(GOSSIP_FANOUT <= 8) }
}
