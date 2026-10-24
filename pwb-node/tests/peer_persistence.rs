//! Peer memory across restarts: a node re-dials the peers it authenticated
//! before it stopped, and forgets remembered addresses that no longer answer.

use std::path::PathBuf;
use std::time::Duration;

use pwb_core::crypto::Keypair;
use pwb_core::net::{NodeConfig, NodeEngine};
use pwb_node::peers;
use pwb_node::persist::{self, StoredPeer};

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

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "pwb-peers-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test]
async fn restart_rejoins_remembered_peer_and_drops_dead_ones() {
    let dir = temp_dir("restore");
    let store = dir.join("peers.json");

    let kp_a = Keypair::generate();
    let kp_b = Keypair::generate();
    let kp_c = Keypair::generate();
    let a = engine(kp_a.clone());
    let b = engine(kp_b.clone());
    a.start().await.unwrap();
    b.start().await.unwrap();
    let addr_a = a.local_addr().unwrap().unwrap();

    // B learns A the way a live node does: an authenticated dial.
    b.dial(addr_a, kp_a.public, Duration::from_millis(1500))
        .await
        .expect("dial a");
    let remembered = peers::snapshot(&b, &b.public_key());
    assert_eq!(remembered.len(), 1);
    persist::save_peers(&store, &remembered).unwrap();

    // An address that is remembered but no longer listening: reserve a port,
    // then release it so nothing answers the probe.
    let dead_socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let dead_addr = dead_socket.local_addr().unwrap();
    drop(dead_socket);
    let dead = StoredPeer {
        public_key: Keypair::generate().public,
        address: dead_addr,
        last_seen_unix: 1,
    };
    let mut stored = persist::load_peers(&store);
    stored.push(dead.clone());
    persist::save_peers(&store, &stored).unwrap();

    // A fresh process with an empty peer table restores from disk.
    let c = engine(kp_c.clone());
    c.start().await.unwrap();
    assert!(c.peer_table().lock().unwrap().is_empty());

    let reconnected = peers::restore(&c, &store).await;
    assert_eq!(
        reconnected, 1,
        "the live peer reconnects, the dead one does not"
    );
    assert!(c.get_peer(&kp_a.public).is_some());
    assert!(
        c.get_peer(&dead.public_key).is_none(),
        "a failed probe must not leave a live-looking entry"
    );
    // The restored peer now knows the restarted node too.
    assert!(a.get_peer(&kp_c.public).is_some());

    // The dead peer stays in the remembered store for a future attempt, while
    // the live peer is recorded as seen now.
    let merged = peers::merged(&c, &c.public_key(), &store);
    assert_eq!(merged.len(), 2);

    for node in [&a, &b, &c] {
        node.stop().await;
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
