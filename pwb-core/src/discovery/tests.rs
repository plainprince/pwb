use super::constants::BEACON_VERSION;
use super::*;
use crate::crypto::{Keypair, PublicKey};
use crate::net::PeerInfo;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;

async fn service_on(listen_port: u16) -> DiscoveryService {
    DiscoveryService::bind(
        DiscoveryConfig {
            listen_port,
            bind_port: 0,
            ..DiscoveryConfig::default()
        },
        Keypair::generate(),
    )
    .await
    .expect("bind ephemeral discovery port")
}

/// Turn a bound wildcard address into the loopback address of the same family.
///
/// A discovery socket binds `0.0.0.0` / `::`, and sending a datagram to a
/// wildcard destination is undefined in practice: Linux happens to route it
/// back to loopback, Windows rejects it with `WSAEADDRNOTAVAIL`. The unicast
/// tests want a real local delivery, so they dial loopback explicitly.
fn loopback(addr: SocketAddr) -> SocketAddr {
    match addr.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => {
            SocketAddr::from((Ipv4Addr::LOCALHOST, addr.port()))
        }
        IpAddr::V6(ip) if ip.is_unspecified() => {
            SocketAddr::from((Ipv6Addr::LOCALHOST, addr.port()))
        }
        _ => addr,
    }
}

#[test]
fn test_beacon_sign_encode_decode_verify_roundtrip() {
    let keypair = Keypair::generate();
    let t = 1_700_000_000u64;
    let beacon = Beacon::sign(&keypair, 8123, t);
    assert_eq!(beacon.public_key, keypair.public);
    assert_eq!(beacon.encode().len(), BEACON_SIZE);

    let decoded = Beacon::decode(&beacon.encode()).expect("wire roundtrip");
    assert_eq!(decoded, beacon);
    decoded.verify(t, BEACON_MAX_AGE).expect("fresh + signed");
    decoded
        .verify(t + 100, BEACON_MAX_AGE)
        .expect("within freshness window");
    decoded
        .verify(t - 100, BEACON_MAX_AGE)
        .expect("small backward clock skew tolerated");
}

#[test]
fn test_beacon_decode_rejects_bad_framing() {
    let keypair = Keypair::generate();
    let bytes = Beacon::sign(&keypair, 8123, 1_700_000_000).encode();

    assert_eq!(
        Beacon::decode(&bytes[..BEACON_SIZE - 1]),
        Err(BeaconError::BadSize {
            got: BEACON_SIZE - 1
        })
    );
    assert_eq!(
        Beacon::decode(&[0u8; BEACON_SIZE + 1]),
        Err(BeaconError::BadSize {
            got: BEACON_SIZE + 1
        })
    );

    let mut bad_magic = bytes;
    bad_magic[0] = b'X';
    assert_eq!(Beacon::decode(&bad_magic), Err(BeaconError::BadMagic));

    let mut bad_version = bytes;
    bad_version[4] = BEACON_VERSION + 1;
    assert_eq!(
        Beacon::decode(&bad_version),
        Err(BeaconError::UnsupportedVersion(BEACON_VERSION + 1))
    );
}

#[test]
fn test_beacon_verify_rejects_tampering_and_bad_timing() {
    let keypair = Keypair::generate();
    let t = 1_700_000_000u64;
    let beacon = Beacon::sign(&keypair, 8123, t);

    // Every signed field breaks the signature when altered.
    let mut tampered = beacon.clone();
    tampered.listen_port = 9;
    assert_eq!(
        tampered.verify(t, BEACON_MAX_AGE),
        Err(BeaconError::BadSignature)
    );

    let mut tampered = beacon.clone();
    tampered.timestamp = t + 1;
    assert_eq!(
        tampered.verify(t, BEACON_MAX_AGE),
        Err(BeaconError::BadSignature)
    );

    let mut tampered = beacon.clone();
    tampered.signature.0[0] ^= 0x01;
    assert_eq!(
        tampered.verify(t, BEACON_MAX_AGE),
        Err(BeaconError::BadSignature)
    );

    // Freshness rejects stale and future-dated beacons (checked before
    // the signature, so replayed old datagrams fail cheaply).
    let max_age_secs = BEACON_MAX_AGE.as_secs();
    assert_eq!(
        beacon.verify(t + max_age_secs + 1, BEACON_MAX_AGE),
        Err(BeaconError::Stale {
            age_secs: max_age_secs + 1,
            max_age_secs
        })
    );
    let future = Beacon::sign(&keypair, 8123, t + max_age_secs + 1);
    assert_eq!(
        future.verify(t, BEACON_MAX_AGE),
        Err(BeaconError::FromFuture {
            skew_secs: max_age_secs + 1,
            max_age_secs
        })
    );
}

#[tokio::test]
async fn test_announce_fanout_sends() {
    let service = service_on(8123).await;
    // At least one of broadcast / multicast must accept the datagram on any
    // usable network stack; both failing means discovery is dead. Each bound
    // family (IPv4, IPv6) fans out to two targets, so the ceiling is four.
    let sent = service
        .announce(unix_time())
        .await
        .expect("fanout sends at least one datagram");
    assert!((1..=4).contains(&sent));
}

#[tokio::test]
async fn test_ipv4_socket_is_allowed_to_broadcast() {
    // The fanout's 255.255.255.255 datagram needs `SO_BROADCAST`; without it
    // the kernel refuses it (`EACCES` on Unix, `WSAEACCES` on Windows) and
    // `announce` quietly degrades to multicast only — a failure the fanout
    // deliberately tolerates, so this flag is the one thing that would go
    // missing without a test noticing.
    let service = service_on(8123).await;
    assert_eq!(
        service.broadcast_enabled().map(Result::unwrap),
        Some(true),
        "the IPv4 discovery socket must be permitted to send broadcast beacons"
    );
}

#[tokio::test]
async fn test_ipv6_unicast_beacon_discovery_merges_peer() {
    let a = service_on(8123).await;
    let b = service_on(8124).await;
    let Some(addr_b) = b.local_addr_v6() else {
        // A host without an IPv6 stack has no IPv6 path to test; the service
        // deliberately carries on with IPv4 alone.
        return;
    };
    assert_eq!(
        addr_b.ip(),
        std::net::IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED)
    );
    let t = unix_time();

    // A beacon sent to the IPv6 socket is validated and merged like any
    // other, and the peer is recorded at its real IPv6 source address.
    a.send_beacon_to(loopback(addr_b), t)
        .await
        .expect("unicast v6 beacon");
    let seen = b.recv_beacon(t, Instant::now()).await.expect("valid");
    assert_eq!(seen, a.public_key());
    let peer = b.peer(&seen).expect("merged into peer table");
    assert_eq!(peer.address.port(), 8123, "advertised service port");
    assert_eq!(
        peer.address.ip(),
        std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST),
        "source IP of the IPv6 beacon"
    );
}

#[tokio::test]
async fn test_unicast_beacon_discovery_merges_peer() {
    let a = service_on(8123).await;
    let b = service_on(8124).await;
    let addr_b = loopback(b.local_addr().unwrap());
    let t = unix_time();

    a.send_beacon_to(loopback(addr_b), t)
        .await
        .expect("unicast beacon");
    let seen = b.recv_beacon(t, Instant::now()).await.expect("valid");
    assert_eq!(seen, a.public_key());

    let peer = b.peer(&seen).expect("merged into peer table");
    assert_eq!(peer.address.port(), 8123, "advertised service port");
    assert_eq!(
        peer.address.ip(),
        std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        "source IP of the beacon"
    );

    // A re-announce refreshes the same entry rather than duplicating it.
    a.send_beacon_to(loopback(addr_b), t + 5).await.unwrap();
    b.recv_beacon(t + 5, Instant::now()).await.unwrap();
    assert_eq!(b.peer_table().lock().unwrap().len(), 1);
    assert!(b.peer(&seen).is_some());
}

#[tokio::test]
async fn test_recv_rejects_stale_garbage_and_forged_beacons() {
    let a = service_on(8123).await;
    let b = service_on(8124).await;
    let addr_b = loopback(b.local_addr().unwrap());
    let t = unix_time();

    // Stale replay of an otherwise valid beacon.
    a.send_beacon_to(loopback(addr_b), t).await.unwrap();
    let err = b
        .recv_beacon(t + BEACON_MAX_AGE.as_secs() + 10, Instant::now())
        .await
        .expect_err("stale beacon rejected");
    assert!(matches!(
        err,
        DiscoveryError::Beacon(BeaconError::Stale { .. })
    ));
    assert!(b.peer_table().lock().unwrap().is_empty());

    // Non-beacon garbage.
    let raw = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    raw.send_to(b"hello", loopback(addr_b)).await.unwrap();
    let err = b.recv_beacon(t, Instant::now()).await.expect_err("garbage");
    assert!(matches!(
        err,
        DiscoveryError::Beacon(BeaconError::BadSize { .. })
    ));

    // Forged beacon: valid frame, tampered signature.
    let mut forged = a.beacon(t);
    forged.signature.0[0] ^= 0x01;
    raw.send_to(&forged.encode(), loopback(addr_b))
        .await
        .unwrap();
    let err = b.recv_beacon(t, Instant::now()).await.expect_err("forged");
    assert!(matches!(
        err,
        DiscoveryError::Beacon(BeaconError::BadSignature)
    ));
    assert!(b.peer_table().lock().unwrap().is_empty());
}

#[tokio::test]
async fn test_address_rebind_grace_blocks_replay_redirect() {
    // One identity announced from two sockets — models a replayed beacon
    // arriving from an attacker's address.
    let keypair = Keypair::generate();
    let config = |listen_port| DiscoveryConfig {
        listen_port,
        bind_port: 0,
        ..DiscoveryConfig::default()
    };
    let original = DiscoveryService::bind(config(8123), keypair.clone())
        .await
        .unwrap();
    let impostor = DiscoveryService::bind(config(8124), keypair.clone())
        .await
        .unwrap();
    let listener = service_on(9000).await;
    let addr_listener = loopback(listener.local_addr().unwrap());
    let t = unix_time();

    // First sighting: address recorded.
    original
        .send_beacon_to(loopback(addr_listener), t)
        .await
        .unwrap();
    let key = listener.recv_beacon(t, Instant::now()).await.unwrap();
    assert_eq!(listener.peer(&key).unwrap().address.port(), 8123);

    // Within the grace window the impostor refreshes liveness only —
    // the sticky address cannot be redirected.
    impostor
        .send_beacon_to(loopback(addr_listener), t + 1)
        .await
        .unwrap();
    listener.recv_beacon(t + 1, Instant::now()).await.unwrap();
    assert_eq!(
        listener.peer(&key).unwrap().address.port(),
        8123,
        "replayed beacon must not rebind the address"
    );

    // After the grace window the new address is accepted (legitimate
    // DHCP/host moves eventually take effect).
    let later = Instant::now() + ADDR_REBIND_GRACE + Duration::from_secs(1);
    impostor
        .send_beacon_to(loopback(addr_listener), t + 2)
        .await
        .unwrap();
    listener.recv_beacon(t + 2, later).await.unwrap();
    assert_eq!(
        listener.peer(&key).unwrap().address.port(),
        8124,
        "address migrates after the grace window"
    );
}

#[tokio::test]
async fn test_spawn_loop_announces_and_merges_into_shared_table() {
    let shared: Arc<Mutex<HashMap<PublicKey, PeerInfo>>> = Arc::new(Mutex::new(HashMap::new()));
    let announcer = DiscoveryService::bind_with_table(
        DiscoveryConfig {
            listen_port: 8123,
            bind_port: 0,
            ..DiscoveryConfig::default()
        },
        Keypair::generate(),
        Arc::clone(&shared),
    )
    .await
    .unwrap();
    let addr_announcer = loopback(announcer.local_addr().unwrap());
    let handle = announcer.spawn();

    let b = service_on(8124).await;
    let key_b = b.public_key();
    b.send_beacon_to(loopback(addr_announcer), unix_time())
        .await
        .unwrap();

    // The spawned loop validates and merges within a short deadline
    // (its immediate first announce tick runs concurrently).
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if shared.lock().unwrap().contains_key(&key_b) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "spawned discovery loop did not merge the beacon"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let peer = shared.lock().unwrap().get(&key_b).cloned().expect("peer");
    assert_eq!(peer.address.port(), 8124, "b's advertised service port");

    handle.abort();
}

#[tokio::test]
async fn test_beacon_display_messages() {
    assert_eq!(
        BeaconError::BadSize { got: 7 }.to_string(),
        format!("beacon must be exactly {BEACON_SIZE} bytes, got 7")
    );
    assert_eq!(
        BeaconError::BadMagic.to_string(),
        "not a PlainWeb discovery beacon (bad magic)"
    );
    assert_eq!(
        BeaconError::UnsupportedVersion(2).to_string(),
        "unsupported beacon version 2"
    );
    assert_eq!(
        BeaconError::Stale {
            age_secs: 500,
            max_age_secs: 120
        }
        .to_string(),
        "beacon is 500s old (max 120s freshness window)"
    );
    assert_eq!(
        BeaconError::FromFuture {
            skew_secs: 42,
            max_age_secs: 120
        }
        .to_string(),
        "beacon is 42s in the future (max 120s clock skew)"
    );
    assert_eq!(
        BeaconError::BadSignature.to_string(),
        "beacon signature does not verify"
    );
}

#[tokio::test]
async fn test_local_addr_and_peer_accessor_shapes() {
    let service = service_on(8123).await;
    let addr: SocketAddr = service.local_addr().unwrap();
    assert!(addr.port() != 0, "ephemeral port assigned");
    assert!(service.peer(&service.public_key()).is_none());
    assert_eq!(
        service.peer_table().lock().unwrap().len(),
        0,
        "starts empty until beacons arrive"
    );
}
