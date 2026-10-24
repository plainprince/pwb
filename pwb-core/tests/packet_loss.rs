//! Packet-loss validation over real loopback UDP.
//!
//! A real lossy internet path cannot be created in a unit test, but a UDP shim
//! can drop the first datagrams in each direction deterministically. That
//! exercises the retry behavior that a single unacknowledged probe would miss:
//! a public bootstrap or peer is not unreachable just because one datagram was
//! lost.

use std::net::SocketAddr;
use std::time::Duration;

use pwb_core::crypto::Keypair;
use pwb_core::net::{BootstrapPeer, NodeConfig, NodeEngine};
use tokio::net::UdpSocket;

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

/// Forward datagrams between `a` and `b`, dropping the first `drop_first`
/// datagrams in each direction. The shim owns the socket both sides see, so an
/// inbound datagram is attributed to the shim's address (as it would be behind
/// a NAT or relay) and replies come back through the same path.
async fn lossy_shim(
    a: SocketAddr,
    b: SocketAddr,
    drop_first: usize,
) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let socket = UdpSocket::bind("127.0.0.1:0").await.expect("shim bind");
    let addr = socket.local_addr().expect("shim addr");
    let handle = tokio::spawn(async move {
        let mut buf = [0u8; 2048];
        let mut dropped_toward_b = 0usize;
        let mut dropped_toward_a = 0usize;
        loop {
            let Ok((len, from)) = socket.recv_from(&mut buf).await else {
                break;
            };
            let toward_b = from != b;
            if toward_b {
                if dropped_toward_b < drop_first {
                    dropped_toward_b += 1;
                    continue;
                }
            } else if dropped_toward_a < drop_first {
                dropped_toward_a += 1;
                continue;
            }
            let destination = if toward_b { b } else { a };
            let _ = socket.send_to(&buf[..len], destination).await;
        }
    });
    (addr, handle)
}

#[tokio::test]
async fn dial_retries_when_the_first_probes_are_lost() {
    let kp_a = Keypair::generate();
    let kp_b = Keypair::generate();
    let (engine_a, addr_a) = start_engine(kp_a.clone()).await;
    let (engine_b, addr_b) = start_engine(kp_b.clone()).await;

    // Both initial Pings and the first Pong are dropped. A single-probe dial
    // would time out here even though B is alive and listening.
    let (shim, shim_handle) = lossy_shim(addr_a, addr_b, 2).await;
    let peer = engine_a
        .dial(shim, kp_b.public, Duration::from_secs(2))
        .await
        .expect("dial retries through the lost probes");
    assert_eq!(peer.public_key, kp_b.public);
    assert!(
        engine_a.metrics().snapshot().peer_dials >= 1,
        "the successful dial is counted"
    );

    shim_handle.abort();
    engine_a.stop().await;
    engine_b.stop().await;
}
