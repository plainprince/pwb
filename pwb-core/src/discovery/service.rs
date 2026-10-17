//! The LAN discovery service: bind, announce, receive, and merge.

use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;

use crate::crypto::{Keypair, PublicKey};
use crate::net::PeerInfo;

use super::beacon::{unix_time, Beacon};
use super::constants::{
    ADDR_REBIND_GRACE, BEACON_INTERVAL, BEACON_MAX_AGE, BEACON_SIZE, BROADCAST_ADDR,
    DISCOVERY_PORT, MULTICAST_GROUP,
};
use super::error::DiscoveryError;

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
