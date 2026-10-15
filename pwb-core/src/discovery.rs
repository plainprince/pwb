//! LAN auto-discovery via UDP broadcast beacons and IP multicast (TODO §3.1).
//!
//! Every node periodically announces itself with a small, Ed25519-signed
//! **beacon** on the local network, and listens for the same:
//!
//! - **Broadcast fanout** — beacons go to `255.255.255.255:7922`, reaching
//!   every host on the LAN subnet.
//! - **Multicast group** — beacons are also sent to (and received from)
//!   `239.255.0.1:7922`; only sockets that joined the group exchange this
//!   traffic, so the group works independently of subnet broadcast rules.
//! - **Authenticated** — each beacon carries the announcer's Ed25519 public
//!   key and a signature over magic/version/port/timestamp/key, so a peer
//!   identity cannot be forged or altered in flight.
//! - **Fresh** — timestamps outside [`DiscoveryConfig::max_age`] are
//!   rejected in both directions (stale replays and future-dated floods).
//!
//! Valid beacons merge into a peer table — the same [`crate::net::PeerInfo`]
//! table [`crate::net::NodeEngine`] uses. Bind with
//! [`DiscoveryService::bind_with_table`] passing `engine.peer_table()` and
//! LAN peers appear to the engine immediately; [`DiscoveryService::spawn`]
//! then runs announce + receive as one background task.
//!
//! Address rebinding is rate-limited: a beacon claiming a *different*
//! address for a recently seen key refreshes `last_seen` but keeps the old
//! address until [`ADDR_REBIND_GRACE`] elapses, so a replayed beacon cannot
//! redirect peer traffic to an attacker's socket.

use crate::crypto::{Keypair, PublicKey, Signature};
use crate::net::PeerInfo;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::fmt;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tokio::net::UdpSocket;

/// Discovery port shared by every PlainWeb node (beacons + multicast).
pub const DISCOVERY_PORT: u16 = 7922;

/// IPv4 multicast group for discovery beacons (TODO §3.1).
pub const MULTICAST_GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 0, 1);

/// IPv4 limited broadcast address (whole subnet).
pub const BROADCAST_ADDR: Ipv4Addr = Ipv4Addr::new(255, 255, 255, 255);

/// Fixed beacon wire size: magic(4) + version(1) + kind(1) + port(2) +
/// timestamp(8) + public key(32) + signature(64).
pub const BEACON_SIZE: usize = 112;

/// Bytes covered by the beacon signature (`magic` ..= `public key`).
const BEACON_SIGNED_LEN: usize = 48;

/// Default announce interval.
pub const BEACON_INTERVAL: Duration = Duration::from_secs(30);

/// Default freshness window for beacon timestamps (both directions).
pub const BEACON_MAX_AGE: Duration = Duration::from_secs(120);

/// How long a known peer's address is sticky: within this window after the
/// last sighting, a beacon claiming a *new* address for the same key
/// refreshes `last_seen` only. Prevents replayed beacons from redirecting
/// traffic to an attacker-controlled socket. Mirrors `NodeEngine`'s
/// 5-minute stale-peer retention.
pub const ADDR_REBIND_GRACE: Duration = Duration::from_secs(300);

const BEACON_MAGIC: [u8; 4] = *b"PWBD";
const BEACON_VERSION: u8 = 1;
const BEACON_KIND_ANNOUNCE: u8 = 0;

/// Errors decoding or validating a discovery beacon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BeaconError {
    /// Datagram was not exactly [`BEACON_SIZE`] bytes.
    BadSize { got: usize },
    /// Missing the `PWBD` magic.
    BadMagic,
    /// Unknown beacon format version.
    UnsupportedVersion(u8),
    /// Beacon timestamp is older than the freshness window.
    Stale { age_secs: u64, max_age_secs: u64 },
    /// Beacon timestamp is ahead of the local clock (flood/replay attempt).
    FromFuture { skew_secs: u64, max_age_secs: u64 },
    /// Signature does not verify for the embedded public key.
    BadSignature,
}

impl fmt::Display for BeaconError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BeaconError::BadSize { got } => {
                write!(f, "beacon must be exactly {BEACON_SIZE} bytes, got {got}")
            }
            BeaconError::BadMagic => write!(f, "not a PlainWeb discovery beacon (bad magic)"),
            BeaconError::UnsupportedVersion(v) => write!(f, "unsupported beacon version {v}"),
            BeaconError::Stale {
                age_secs,
                max_age_secs,
            } => write!(
                f,
                "beacon is {age_secs}s old (max {max_age_secs}s freshness window)"
            ),
            BeaconError::FromFuture {
                skew_secs,
                max_age_secs,
            } => write!(
                f,
                "beacon is {skew_secs}s in the future (max {max_age_secs}s clock skew)"
            ),
            BeaconError::BadSignature => write!(f, "beacon signature does not verify"),
        }
    }
}

impl std::error::Error for BeaconError {}

/// Errors from discovery socket operations.
#[derive(Debug, Error)]
pub enum DiscoveryError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Beacon(#[from] BeaconError),
}

/// Current Unix time in seconds (for [`Beacon::sign`] and the service
/// methods; tests pass explicit timestamps instead).
pub fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The signed portion of a beacon: magic, version, kind, port, timestamp,
/// public key.
fn beacon_prefix(
    listen_port: u16,
    timestamp: u64,
    public_key: &PublicKey,
) -> [u8; BEACON_SIGNED_LEN] {
    let mut prefix = [0u8; BEACON_SIGNED_LEN];
    prefix[0..4].copy_from_slice(&BEACON_MAGIC);
    prefix[4] = BEACON_VERSION;
    prefix[5] = BEACON_KIND_ANNOUNCE;
    prefix[6..8].copy_from_slice(&listen_port.to_be_bytes());
    prefix[8..16].copy_from_slice(&timestamp.to_be_bytes());
    prefix[16..48].copy_from_slice(&public_key.0);
    prefix
}

/// A signed discovery beacon: "node `public_key` listens on `listen_port`
/// at time `timestamp`".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Beacon {
    /// The announcing node's Ed25519 identity.
    pub public_key: PublicKey,
    /// The port the node's main service listens on (beacons themselves
    /// travel on [`DISCOVERY_PORT`]); peers address this port at the
    /// beacon's source IP.
    pub listen_port: u16,
    /// Unix seconds when the beacon was signed.
    pub timestamp: u64,
    /// Ed25519 signature over [`beacon_prefix`] of these fields.
    pub signature: Signature,
}

impl Beacon {
    /// Sign a fresh beacon for `keypair` at `now_unix`.
    pub fn sign(keypair: &Keypair, listen_port: u16, now_unix: u64) -> Self {
        let prefix = beacon_prefix(listen_port, now_unix, &keypair.public);
        Self {
            public_key: keypair.public,
            listen_port,
            timestamp: now_unix,
            signature: keypair.sign(&prefix),
        }
    }

    /// Fixed-size wire bytes (exactly [`BEACON_SIZE`]).
    pub fn encode(&self) -> [u8; BEACON_SIZE] {
        let mut out = [0u8; BEACON_SIZE];
        out[..BEACON_SIGNED_LEN].copy_from_slice(&beacon_prefix(
            self.listen_port,
            self.timestamp,
            &self.public_key,
        ));
        out[BEACON_SIGNED_LEN..].copy_from_slice(&self.signature.0);
        out
    }

    /// Decode wire bytes (size/magic/version only — callers must still
    /// [`Beacon::verify`] for freshness and signature).
    pub fn decode(bytes: &[u8]) -> Result<Self, BeaconError> {
        if bytes.len() != BEACON_SIZE {
            return Err(BeaconError::BadSize { got: bytes.len() });
        }
        if bytes[..4] != BEACON_MAGIC {
            return Err(BeaconError::BadMagic);
        }
        if bytes[4] != BEACON_VERSION {
            return Err(BeaconError::UnsupportedVersion(bytes[4]));
        }
        let listen_port = u16::from_be_bytes(bytes[6..8].try_into().expect("2-byte slice"));
        let timestamp = u64::from_be_bytes(bytes[8..16].try_into().expect("8-byte slice"));
        let mut public_key = [0u8; 32];
        public_key.copy_from_slice(&bytes[16..48]);
        let mut signature = [0u8; 64];
        signature.copy_from_slice(&bytes[48..BEACON_SIZE]);
        Ok(Self {
            public_key: PublicKey(public_key),
            listen_port,
            timestamp,
            signature: Signature(signature),
        })
    }

    /// Validate freshness against `now_unix ± max_age`, then the signature
    /// over the encoded prefix. Freshness is checked first (cheap) so
    /// replayed old datagrams never reach signature verification.
    pub fn verify(&self, now_unix: u64, max_age: Duration) -> Result<(), BeaconError> {
        let max_age_secs = max_age.as_secs();
        if now_unix >= self.timestamp {
            let age_secs = now_unix - self.timestamp;
            if age_secs > max_age_secs {
                return Err(BeaconError::Stale {
                    age_secs,
                    max_age_secs,
                });
            }
        } else {
            let skew_secs = self.timestamp - now_unix;
            if skew_secs > max_age_secs {
                return Err(BeaconError::FromFuture {
                    skew_secs,
                    max_age_secs,
                });
            }
        }
        let prefix = beacon_prefix(self.listen_port, self.timestamp, &self.public_key);
        if !self.signature.verify(&self.public_key, &prefix) {
            return Err(BeaconError::BadSignature);
        }
        Ok(())
    }
}

/// Discovery addressing and timing configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryConfig {
    /// The port the node's main service listens on — advertised in beacons
    /// (set this from your `NodeConfig::listen_addr`).
    pub listen_port: u16,
    /// UDP port the discovery socket binds: [`DISCOVERY_PORT`] in
    /// production, `0` for an ephemeral port in tests.
    pub bind_port: u16,
    /// How often [`DiscoveryService::spawn`] re-announces.
    pub beacon_interval: Duration,
    /// Beacon freshness window accepted on receive.
    pub max_age: Duration,
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self {
            listen_port: DISCOVERY_PORT,
            bind_port: DISCOVERY_PORT,
            beacon_interval: BEACON_INTERVAL,
            max_age: BEACON_MAX_AGE,
        }
    }
}

/// LAN discovery service: signed beacon announce + validated receive/merge.
pub struct DiscoveryService {
    socket: UdpSocket,
    keypair: Keypair,
    config: DiscoveryConfig,
    peer_table: Arc<Mutex<HashMap<PublicKey, PeerInfo>>>,
}

impl DiscoveryService {
    /// Bind on the discovery port with a private peer table (inspect it with
    /// [`Self::peer_table`]).
    pub async fn bind(config: DiscoveryConfig, keypair: Keypair) -> std::io::Result<Self> {
        let peer_table = Arc::new(Mutex::new(HashMap::new()));
        Self::bind_with_table(config, keypair, peer_table).await
    }

    /// Bind sharing an existing peer table — pass
    /// `NodeEngine::peer_table()` so LAN discoveries appear to the engine
    /// immediately.
    pub async fn bind_with_table(
        config: DiscoveryConfig,
        keypair: Keypair,
        peer_table: Arc<Mutex<HashMap<PublicKey, PeerInfo>>>,
    ) -> std::io::Result<Self> {
        let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, config.bind_port)).await?;
        // Multicast membership is best-effort: a host without multicast
        // routing still discovers peers via broadcast and unicast beacons.
        let _ = socket.join_multicast_v4(MULTICAST_GROUP, Ipv4Addr::UNSPECIFIED);
        let _ = socket.set_multicast_loop_v4(true);
        Ok(Self {
            socket,
            keypair,
            config,
            peer_table,
        })
    }

    /// This node's identity as announced in beacons.
    pub fn public_key(&self) -> PublicKey {
        self.keypair.public
    }

    /// The discovery socket's bound address.
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    /// The shared peer table (same handle as `NodeEngine::peer_table`).
    pub fn peer_table(&self) -> Arc<Mutex<HashMap<PublicKey, PeerInfo>>> {
        Arc::clone(&self.peer_table)
    }

    /// Look up a discovered peer.
    pub fn peer(&self, key: &PublicKey) -> Option<PeerInfo> {
        self.peer_table.lock().unwrap().get(key).cloned()
    }

    /// Sign this node's beacon for `now_unix`.
    pub fn beacon(&self, now_unix: u64) -> Beacon {
        Beacon::sign(&self.keypair, self.config.listen_port, now_unix)
    }

    /// Fan one beacon out to the subnet broadcast address and the multicast
    /// group. Tolerant: succeeds when at least one datagram leaves (a host
    /// may lack multicast routing or broadcast permission but not both).
    pub async fn announce(&self, now_unix: u64) -> Result<usize, DiscoveryError> {
        let bytes = self.beacon(now_unix).encode();
        let targets = [
            SocketAddr::from((BROADCAST_ADDR, DISCOVERY_PORT)),
            SocketAddr::from((MULTICAST_GROUP, DISCOVERY_PORT)),
        ];
        let mut sent = 0usize;
        let mut last_err = None;
        for target in targets {
            match self.socket.send_to(&bytes, target).await {
                Ok(_) => sent += 1,
                Err(e) => last_err = Some(e),
            }
        }
        if sent == 0 {
            Err(DiscoveryError::Io(last_err.expect(
                "both fanout sends failed, so an error was recorded",
            )))
        } else {
            Ok(sent)
        }
    }

    /// Send one beacon to an explicit address (targeted/unicast discovery,
    /// and the deterministic path tests use).
    pub async fn send_beacon_to(
        &self,
        addr: SocketAddr,
        now_unix: u64,
    ) -> Result<(), DiscoveryError> {
        let bytes = self.beacon(now_unix).encode();
        self.socket.send_to(&bytes, addr).await?;
        Ok(())
    }

    /// Receive one datagram, validate it as a beacon, and merge the
    /// announcer into the peer table; returns the announced identity.
    ///
    /// Invalid input surfaces as [`DiscoveryError::Beacon`]; callers in the
    /// spawn loop drop those and keep listening.
    pub async fn recv_beacon(
        &self,
        now_unix: u64,
        now: Instant,
    ) -> Result<PublicKey, DiscoveryError> {
        let mut buf = [0u8; BEACON_SIZE + 1];
        let (len, from) = self.socket.recv_from(&mut buf).await?;
        let beacon = Beacon::decode(&buf[..len])?;
        beacon.verify(now_unix, self.config.max_age)?;
        self.merge(&beacon, from, now);
        Ok(beacon.public_key)
    }

    /// Merge a validated beacon into the peer table. The peer is addressed
    /// at the datagram's source IP plus the beacon's advertised
    /// [`Beacon::listen_port`]. An address change for a recently seen key is
    /// held back until [`ADDR_REBIND_GRACE`] passes (see module docs).
    fn merge(&self, beacon: &Beacon, from: SocketAddr, now: Instant) {
        let addr = SocketAddr::new(from.ip(), beacon.listen_port);
        let mut table = self.peer_table.lock().unwrap();
        match table.entry(beacon.public_key) {
            Entry::Vacant(slot) => {
                slot.insert(PeerInfo {
                    public_key: beacon.public_key,
                    last_seen: now,
                    address: addr,
                    reputation: 0,
                });
            }
            Entry::Occupied(mut slot) => {
                let peer = slot.get_mut();
                let age = now.duration_since(peer.last_seen);
                if peer.address == addr || age >= ADDR_REBIND_GRACE {
                    peer.address = addr;
                }
                peer.last_seen = now;
            }
        }
    }

    /// Run the background discovery loop: announce every
    /// [`DiscoveryConfig::beacon_interval`] (the first tick is immediate)
    /// and merge every valid inbound beacon into the peer table. Invalid or
    /// noisy datagrams are dropped; transient socket errors keep the loop
    /// listening. Consumes the service — abort the returned handle to stop.
    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        let mut interval = tokio::time::interval(self.config.beacon_interval);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let _ = self.announce(unix_time()).await;
                    }
                    result = self.recv_beacon(unix_time(), Instant::now()) => {
                        // Invalid beacons and transient IO errors are dropped;
                        // the loop keeps announcing and listening.
                        let _ = result;
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;

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
        // At least one of broadcast / multicast must accept the datagram on
        // any usable network stack; both failing means discovery is dead.
        let sent = service
            .announce(unix_time())
            .await
            .expect("fanout sends at least one datagram");
        assert!((1..=2).contains(&sent));
    }

    #[tokio::test]
    async fn test_unicast_beacon_discovery_merges_peer() {
        let a = service_on(8123).await;
        let b = service_on(8124).await;
        let addr_b = b.local_addr().unwrap();
        let t = unix_time();

        a.send_beacon_to(addr_b, t).await.expect("unicast beacon");
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
        a.send_beacon_to(addr_b, t + 5).await.unwrap();
        b.recv_beacon(t + 5, Instant::now()).await.unwrap();
        assert_eq!(b.peer_table().lock().unwrap().len(), 1);
        assert!(b.peer(&seen).is_some());
    }

    #[tokio::test]
    async fn test_recv_rejects_stale_garbage_and_forged_beacons() {
        let a = service_on(8123).await;
        let b = service_on(8124).await;
        let addr_b = b.local_addr().unwrap();
        let t = unix_time();

        // Stale replay of an otherwise valid beacon.
        a.send_beacon_to(addr_b, t).await.unwrap();
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
        raw.send_to(b"hello", addr_b).await.unwrap();
        let err = b.recv_beacon(t, Instant::now()).await.expect_err("garbage");
        assert!(matches!(
            err,
            DiscoveryError::Beacon(BeaconError::BadSize { .. })
        ));

        // Forged beacon: valid frame, tampered signature.
        let mut forged = a.beacon(t);
        forged.signature.0[0] ^= 0x01;
        raw.send_to(&forged.encode(), addr_b).await.unwrap();
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
        let addr_listener = listener.local_addr().unwrap();
        let t = unix_time();

        // First sighting: address recorded.
        original.send_beacon_to(addr_listener, t).await.unwrap();
        let key = listener.recv_beacon(t, Instant::now()).await.unwrap();
        assert_eq!(listener.peer(&key).unwrap().address.port(), 8123);

        // Within the grace window the impostor refreshes liveness only —
        // the sticky address cannot be redirected.
        impostor.send_beacon_to(addr_listener, t + 1).await.unwrap();
        listener.recv_beacon(t + 1, Instant::now()).await.unwrap();
        assert_eq!(
            listener.peer(&key).unwrap().address.port(),
            8123,
            "replayed beacon must not rebind the address"
        );

        // After the grace window the new address is accepted (legitimate
        // DHCP/host moves eventually take effect).
        let later = Instant::now() + ADDR_REBIND_GRACE + Duration::from_secs(1);
        impostor.send_beacon_to(addr_listener, t + 2).await.unwrap();
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
        let addr_announcer = announcer.local_addr().unwrap();
        let handle = announcer.spawn();

        let b = service_on(8124).await;
        let key_b = b.public_key();
        b.send_beacon_to(addr_announcer, unix_time()).await.unwrap();

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
}
