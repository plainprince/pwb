//! NAT traversal, as close as a single host allows.
//!
//! A real node behind a home router only knows its private LAN address; peers
//! on the internet can only reach it through the router's public mapping. The
//! engine asks a STUN server what that mapping looks like and advertises the
//! answer in its connection link.
//!
//! These tests stand up a local RFC 5389 STUN responder that reports a
//! documentation-range public address (`198.51.100.0/24`) different from the
//! node's loopback socket, then verify the engine:
//!
//! 1. performs the real Binding Request/Response exchange at startup,
//! 2. exposes the mapped address via `public_addr()`,
//! 3. puts the mapped address (not `127.0.0.1`) in its connection link, and
//! 4. lets another node parse that link and register the public address.
//!
//! What is not covered: an actual NAT rewriting packets, port-restricted
//! cone behaviour, hairpinning, and STUN over the public internet. A local
//! responder can prove the client/protocol path, not a router's behaviour.

use pwb_core::crypto::Keypair;
use pwb_core::net::{NodeConfig, NodeEngine};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;
use tokio::net::UdpSocket;

/// STUN magic cookie (RFC 5389 §6).
const MAGIC_COOKIE: u32 = 0x2112_A442;
const BINDING_RESPONSE: u16 = 0x0101;
const ATTR_XOR_MAPPED_ADDRESS: u16 = 0x0020;

fn xor_mapped_address_body(mapped: SocketAddr) -> Vec<u8> {
    let IpAddr::V4(ip) = mapped.ip() else {
        panic!("the STUN client only supports IPv4");
    };
    let ip = u32::from_be_bytes(ip.octets());
    let xor_port = mapped.port() ^ (MAGIC_COOKIE >> 16) as u16;
    let mut body = Vec::with_capacity(8);
    body.push(0x00); // reserved
    body.push(0x01); // IPv4
    body.extend_from_slice(&xor_port.to_be_bytes());
    body.extend_from_slice(&(ip ^ MAGIC_COOKIE).to_be_bytes());
    body
}

/// A minimal STUN server: it echoes the request's transaction id back with an
/// `XOR-MAPPED-ADDRESS` attribute, exactly as a real server behind the NAT
/// would report the source address it observed.
async fn spawn_stun_server(mapped: SocketAddr) -> SocketAddr {
    let socket = UdpSocket::bind("127.0.0.1:0")
        .await
        .expect("bind mock STUN server");
    let addr = socket.local_addr().expect("stun addr");

    tokio::spawn(async move {
        let mut buf = [0u8; 512];
        loop {
            let Ok((len, from)) = socket.recv_from(&mut buf).await else {
                break;
            };
            // Header: type(2) + len(2) + cookie(4) + transaction id(12).
            if len < 20 {
                continue;
            }
            let transaction_id = &buf[8..20];
            let body = xor_mapped_address_body(mapped);
            let attr_len = (4 + body.len()) as u16;

            let mut response = Vec::with_capacity(20 + attr_len as usize);
            response.extend_from_slice(&BINDING_RESPONSE.to_be_bytes());
            response.extend_from_slice(&attr_len.to_be_bytes());
            response.extend_from_slice(&MAGIC_COOKIE.to_be_bytes());
            response.extend_from_slice(transaction_id);
            response.extend_from_slice(&ATTR_XOR_MAPPED_ADDRESS.to_be_bytes());
            response.extend_from_slice(&(body.len() as u16).to_be_bytes());
            response.extend_from_slice(&body);

            let _ = socket.send_to(&response, from).await;
        }
    });

    addr
}

fn engine_config(keypair: Keypair, stun_server: Option<SocketAddr>) -> NodeConfig {
    NodeConfig {
        listen_addr: "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
        keypair,
        bootstrap_peers: Vec::new(),
        stun_server,
        max_peers: 100,
        heartbeat_interval: Duration::from_secs(30),
    }
}

#[tokio::test]
async fn test_engine_learns_its_public_address_and_advertises_it_in_the_link() {
    // A public address in the TEST-NET-2 documentation range, deliberately
    // not the node's loopback socket.
    let mapped: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 23)), 51820);
    let stun_server = spawn_stun_server(mapped).await;

    let keypair = Keypair::generate();
    let engine = NodeEngine::new(engine_config(keypair.clone(), Some(stun_server)));
    engine.start().await.expect("engine starts with STUN");

    assert_eq!(
        engine.public_addr(),
        Some(mapped),
        "the engine must record the XOR-MAPPED-ADDRESS it received"
    );
    assert_ne!(
        engine.public_addr().unwrap().ip(),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        "the mapped address is the public one, not the local bind address"
    );

    // The connection link a user shares (or a QR code encodes) must carry the
    // public address, otherwise internet peers would dial 127.0.0.1.
    let link = engine.generate_connection_link();
    let (link_addr, link_key) =
        NodeEngine::parse_connection_link(&link).expect("generated link parses");
    assert_eq!(link_addr, mapped);
    assert_eq!(link_key, keypair.public);
    assert!(
        link.contains("198.51.100.23:51820"),
        "link should advertise the NAT-mapped address, got {link}"
    );

    // A remote node parses that link and knows where to send datagrams.
    let remote = NodeEngine::new(engine_config(Keypair::generate(), None));
    remote.start().await.expect("remote starts");
    let peer = remote
        .launch_connection(&link)
        .expect("remote accepts the public link");
    assert_eq!(peer.address, mapped);
    assert_eq!(peer.public_key, keypair.public);
    assert_eq!(
        remote.get_peer(&keypair.public).unwrap().address,
        mapped,
        "the public mapping, not the private bind address, is dialed"
    );

    remote.stop().await;
    engine.stop().await;
}
