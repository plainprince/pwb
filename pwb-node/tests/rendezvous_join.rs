//! Simulated public-swarm join over real loopback UDP.
//!
//! A real PlainWeb node on the internet knows exactly one address at startup:
//! a bootstrap/rendezvous peer's `ip:port/pubkey`. Everything else — which
//! other nodes exist, where to send them datagrams, and whether they are
//! actually listening — has to come out of that first contact.
//!
//! These tests reproduce that as closely as a single machine allows:
//!
//! * nodes bind real UDP sockets and exchange signed envelopes over the wire,
//! * new nodes are configured with **only** the rendezvous address (no
//!   connection links, no pre-seeded peer tables),
//! * the rendezvous node gossips its peer table via the same
//!   [`peer_exchange_loop`] the `pwb-node`/`pwb-browser` binaries run,
//! * the newcomer then fetches content from a peer it never dialed directly.
//!
//! What is not covered: real NAT/firewall behaviour, STUN-mapped public
//! addresses, packet loss, and geographically remote peers. Those need
//! multiple hosts and are out of reach for a unit-test suite.

use pwb_core::crypto::{ContentId, Keypair};
use pwb_core::net::{peer_exchange_loop, BootstrapPeer, NodeConfig, NodeEngine};
use pwb_core::spec::{PlainNode, Tag};
use pwb_core::wire::{Bundle, CURRENT_VERSION};
use pwb_node::federation::DistributedSearch;
use pwb_node::host::ArtifactHost;
use pwb_node::search::SearchEngine;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long to wait for datagrams to cross the simulated swarm. Loopback UDP
/// with the engine's 10 ms tick is normally far faster; this is only a ceiling.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(3);

struct Node {
    engine: NodeEngine,
    addr: SocketAddr,
    keypair: Keypair,
}

fn config(keypair: Keypair, bootstrap_peers: Vec<BootstrapPeer>) -> NodeConfig {
    NodeConfig {
        listen_addr: "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
        keypair,
        bootstrap_peers,
        stun_server: None,
        max_peers: 100,
        heartbeat_interval: Duration::from_secs(30),
    }
}

async fn start_engine(keypair: Keypair, bootstrap_peers: Vec<BootstrapPeer>) -> Node {
    let engine = NodeEngine::new(config(keypair.clone(), bootstrap_peers));
    engine.start().await.expect("engine start");
    let addr = engine.local_addr().expect("bound").expect("addr");
    Node {
        engine,
        addr,
        keypair,
    }
}

fn service(node: &Node) -> Arc<DistributedSearch> {
    Arc::new(DistributedSearch::new(
        node.engine.clone(),
        Arc::new(SearchEngine::new()),
        Arc::new(ArtifactHost::new()),
        node.keypair.clone(),
    ))
}

/// Poll `condition` until it holds or `timeout` elapses. Network tests need
/// this instead of a fixed sleep: a loaded CI machine may take longer, while
/// the happy path returns as soon as the datagram lands.
async fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if condition() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
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
        author_signature: None,
        root_node: root,
        script_payload: None,
        components: HashMap::new(),
        modules: HashMap::new(),
    }
    .serialize()
}

/// The full public-network join path, minus the internet:
///
/// ```text
///   B ──dial──► R ◄──dial── C          (only R's address is known in advance)
///   B ◄──PeerExchange── R ──PeerExchange──► C
///   B ──StorePosting──► R, C
///   C ──FetchArtifact──► R, B          (B answers; R does not have the bytes)
/// ```
///
/// Every arrow is a signed UDP datagram between two live engine instances.
#[tokio::test]
async fn test_new_node_joins_swarm_via_rendezvous_and_fetches_from_an_unknown_peer() {
    // Rendezvous node: the only address B and C are given.
    let kp_r = Keypair::generate();
    let rendezvous = start_engine(kp_r.clone(), Vec::new()).await;
    let rendezvous_bootstrap = vec![BootstrapPeer {
        address: rendezvous.addr,
        public_key: kp_r.public,
    }];

    // Two newcomers that have never seen each other's connection links.
    let kp_b = Keypair::generate();
    let node_b = start_engine(kp_b.clone(), rendezvous_bootstrap.clone()).await;
    let kp_c = Keypair::generate();
    let node_c = start_engine(kp_c.clone(), rendezvous_bootstrap).await;

    let service_r = service(&rendezvous);
    let service_b = service(&node_b);
    let service_c = service(&node_c);
    tokio::spawn(Arc::clone(&service_r).run());
    tokio::spawn(Arc::clone(&service_b).run());
    tokio::spawn(Arc::clone(&service_c).run());

    // Join: a Pong proves something is actually listening at the configured
    // address and holds the configured key. A mere table entry would not.
    let joined_b = node_b.engine.connect_bootstrap_verified().await;
    assert_eq!(joined_b.len(), 1, "B joins through its one bootstrap peer");
    assert_eq!(joined_b[0].public_key, kp_r.public);
    assert_eq!(joined_b[0].address, rendezvous.addr);

    let joined_c = node_c.engine.connect_bootstrap_verified().await;
    assert_eq!(joined_c.len(), 1, "C joins through its one bootstrap peer");
    assert_eq!(joined_c[0].public_key, kp_r.public);

    // The rendezvous node learned both newcomers from their inbound Pings —
    // it never dialed either of them.
    assert!(
        wait_until(SETTLE_TIMEOUT, || {
            rendezvous.engine.get_peer(&kp_b.public).is_some()
                && rendezvous.engine.get_peer(&kp_c.public).is_some()
        })
        .await,
        "rendezvous must learn both newcomers from their signed datagrams"
    );

    // The deployed gossip loop (the same function the binaries spawn) shares
    // the rendezvous peer table. B and C learn each other with no direct link.
    let gossip = tokio::spawn(peer_exchange_loop(
        rendezvous.engine.clone(),
        kp_r.public,
        Duration::from_millis(50),
    ));

    assert!(
        wait_until(SETTLE_TIMEOUT, || {
            node_b.engine.get_peer(&kp_c.public).is_some()
                && node_c.engine.get_peer(&kp_b.public).is_some()
        })
        .await,
        "PeerExchange gossip must introduce B and C without a connection link"
    );

    // The gossiped addresses are the peers' real bound sockets, not guesses.
    assert_eq!(
        node_c.engine.get_peer(&kp_b.public).unwrap().address,
        node_b.addr
    );
    assert_eq!(
        node_b.engine.get_peer(&kp_c.public).unwrap().address,
        node_c.addr
    );

    // B publishes an artifact; C, which has never spoken to B, fetches it.
    let bytes = bundle_bytes("Rendezvous Notes", "content reached through gossip");
    let cid = service_b.index_and_publish(&bytes).expect("B indexes");
    assert!(
        !service_c.host().contains(&cid),
        "C starts without the artifact"
    );

    let fetched = service_c
        .fetch(&cid)
        .await
        .expect("C fetches from the gossiped peer");
    assert_eq!(fetched, bytes);
    assert_eq!(ContentId::compute(&fetched), cid);

    gossip.abort();
    node_b.engine.stop().await;
    node_c.engine.stop().await;
    rendezvous.engine.stop().await;
}

/// A bootstrap address that accepts datagrams but never answers is not a
/// successful join. Only the live rendezvous peer counts, even though both
/// were configured identically.
#[tokio::test]
async fn test_unreachable_bootstrap_peer_is_not_counted_as_a_join() {
    let kp_r = Keypair::generate();
    let rendezvous = start_engine(kp_r.clone(), Vec::new()).await;

    // A UDP socket that swallows everything: bound, but no engine behind it,
    // so no Pong ever comes back.
    let blackhole = tokio::net::UdpSocket::bind("127.0.0.1:0")
        .await
        .expect("bind blackhole");
    let blackhole_addr = blackhole.local_addr().unwrap();
    let blackhole_key = Keypair::generate().public;

    let newcomer = start_engine(
        Keypair::generate(),
        vec![
            BootstrapPeer {
                address: blackhole_addr,
                public_key: blackhole_key,
            },
            BootstrapPeer {
                address: rendezvous.addr,
                public_key: kp_r.public,
            },
        ],
    )
    .await;

    let joined = newcomer.engine.connect_bootstrap_verified().await;
    assert_eq!(joined.len(), 1, "only the answering peer is a join");
    assert_eq!(joined[0].public_key, kp_r.public);
    assert_eq!(joined[0].address, rendezvous.addr);
    assert!(
        !joined.iter().any(|peer| peer.public_key == blackhole_key),
        "a silent peer must not be reported as a successful join"
    );

    newcomer.engine.stop().await;
    rendezvous.engine.stop().await;
}

/// A rendezvous node (or anyone who can broadcast) cannot poison the peer
/// table with addresses that can never be dialed: port 0 and unspecified IPs
/// are dropped instead of being stored as if they were reachable peers.
#[tokio::test]
async fn test_peer_exchange_drops_undialable_addresses() {
    use pwb_core::net::NetworkMessage;

    let kp_r = Keypair::generate();
    let rendezvous = start_engine(kp_r.clone(), Vec::new()).await;
    let kp_b = Keypair::generate();
    let node_b = start_engine(
        kp_b.clone(),
        vec![BootstrapPeer {
            address: rendezvous.addr,
            public_key: kp_r.public,
        }],
    )
    .await;
    node_b
        .engine
        .connect_bootstrap_verified()
        .await
        .into_iter()
        .next()
        .expect("B joins R");

    let port_zero = Keypair::generate().public;
    let unspecified = Keypair::generate().public;
    rendezvous
        .engine
        .broadcast(NetworkMessage::PeerExchange {
            peers: vec![
                ("127.0.0.1:0".parse().unwrap(), port_zero),
                ("0.0.0.0:9000".parse().unwrap(), unspecified),
            ],
            sender_pubkey: kp_r.public,
        })
        .expect("R gossips bogus addresses");

    tokio::time::sleep(Duration::from_millis(150)).await;

    assert!(
        node_b.engine.get_peer(&port_zero).is_none(),
        "port 0 is not a dialable peer"
    );
    assert!(
        node_b.engine.get_peer(&unspecified).is_none(),
        "an unspecified address is not a dialable peer"
    );
    // The legitimate rendezvous peer survived the bogus gossip.
    assert!(node_b.engine.get_peer(&kp_r.public).is_some());

    node_b.engine.stop().await;
    rendezvous.engine.stop().await;
}
