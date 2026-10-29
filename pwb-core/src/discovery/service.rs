//! The LAN discovery service: bind, announce, receive, and merge.

use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;

use crate::crypto::{Keypair, PublicKey};
use crate::net::PeerInfo;

use super::beacon::{unix_time, Beacon};
use super::constants::{
    ADDR_REBIND_GRACE, BEACON_INTERVAL, BEACON_MAX_AGE, BEACON_SIZE, BROADCAST_ADDR,
    BROADCAST_ADDR_V6, DISCOVERY_PORT, MULTICAST_GROUP, MULTICAST_GROUP_V6,
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
    /// IPv4 socket: broadcast + administratively scoped multicast fanout.
    v4: Option<UdpSocket>,
    /// IPv6 socket: all-nodes link-local + site-scoped multicast fanout.
    /// Bound v6-only so it can share the discovery port with `v4` on hosts
    /// where `[::]` defaults to dual-stack (a dual bind would collide with
    /// `0.0.0.0` on the same port, and IPv4 beacons would then arrive twice).
    v6: Option<UdpSocket>,
    keypair: Keypair,
    config: DiscoveryConfig,
    peer_table: Arc<Mutex<HashMap<PublicKey, PeerInfo>>>,
}

/// Bind the IPv6 discovery socket on `port`.
///
/// Best-effort by design: a host with no IPv6 stack gets `Err` and the
/// service carries on with its IPv4 socket alone.
async fn bind_v6_socket(port: u16) -> std::io::Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))?;
    // V6-only before bind: see `DiscoveryService::v6`.
    let _ = socket.set_only_v6(true);
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), port).into())?;
    UdpSocket::from_std(socket.into())
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
        // Bind both address families. Either may be unavailable — a v4-less
        // v6-only host or the reverse — so each failure is remembered only to
        // report when *neither* socket could bind.
        let mut v4 = None;
        let mut v4_error = None;
        match UdpSocket::bind((Ipv4Addr::UNSPECIFIED, config.bind_port)).await {
            Ok(socket) => {
                // Sending to 255.255.255.255 needs SO_BROADCAST, otherwise the
                // kernel refuses the datagram: EACCES on Unix, WSAEACCES on
                // Windows. Without it every broadcast fanout failed and only
                // the multicast datagram ever left this socket.
                let _ = socket.set_broadcast(true);
                // Multicast membership is best-effort: a host without
                // multicast routing still discovers peers via broadcast and
                // unicast beacons.
                let _ = socket.join_multicast_v4(MULTICAST_GROUP, Ipv4Addr::UNSPECIFIED);
                let _ = socket.set_multicast_loop_v4(true);
                v4 = Some(socket);
            }
            Err(error) => v4_error = Some(error),
        }
        let mut v6 = None;
        let mut v6_error = None;
        match bind_v6_socket(config.bind_port).await {
            Ok(socket) => {
                // Group membership and loopback are best-effort here too: a
                // host without multicast routing still gets unicast beacons,
                // and the all-nodes group is joined by every interface
                // anyway.
                let _ = socket.join_multicast_v6(&MULTICAST_GROUP_V6, 0);
                let _ = socket.set_multicast_loop_v6(true);
                v6 = Some(socket);
            }
            Err(error) => v6_error = Some(error),
        }
        if v4.is_none() && v6.is_none() {
            return Err(v4_error
                .or(v6_error)
                .expect("both discovery binds failed, an error was recorded"));
        }
        Ok(Self {
            v4,
            v6,
            keypair,
            config,
            peer_table,
        })
    }

    /// This node's identity as announced in beacons.
    pub fn public_key(&self) -> PublicKey {
        self.keypair.public
    }

    /// The discovery socket's bound address: the IPv4 socket when the host
    /// has one (what unicast tests and IPv4 peers dial), otherwise the IPv6
    /// socket.
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.v4
            .as_ref()
            .or(self.v6.as_ref())
            .expect("bind keeps at least one discovery socket")
            .local_addr()
    }

    /// The IPv6 discovery socket's bound address, when the host has one.
    pub fn local_addr_v6(&self) -> Option<SocketAddr> {
        self.v6
            .as_ref()
            .map(UdpSocket::local_addr)
            .transpose()
            .ok()
            .flatten()
    }

    /// `SO_BROADCAST` on the IPv4 socket, when the host has one. Test-only:
    /// the subnet-broadcast half of [`Self::announce`] is refused by the
    /// kernel without it, and the fanout tolerates that refusal — so nothing
    /// but a test would notice the flag going missing.
    #[cfg(test)]
    pub(crate) fn broadcast_enabled(&self) -> Option<std::io::Result<bool>> {
        self.v4.as_ref().map(|socket| socket.broadcast())
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

    /// Fan one beacon out on every bound socket: to the subnet broadcast
    /// address and the multicast group over IPv4, and to the all-nodes link
    /// local address and the site-scoped group over IPv6. Tolerant:
    /// succeeds when at least one datagram leaves (a host may lack multicast
    /// routing, broadcast permission, or one address family, but not all of
    /// them).
    pub async fn announce(&self, now_unix: u64) -> Result<usize, DiscoveryError> {
        let bytes = self.beacon(now_unix).encode();
        let v4_targets = [
            SocketAddr::from((BROADCAST_ADDR, DISCOVERY_PORT)),
            SocketAddr::from((MULTICAST_GROUP, DISCOVERY_PORT)),
        ];
        let v6_targets = [
            SocketAddr::from((BROADCAST_ADDR_V6, DISCOVERY_PORT)),
            SocketAddr::from((MULTICAST_GROUP_V6, DISCOVERY_PORT)),
        ];
        let mut sent = 0usize;
        let mut last_err = None;
        for (socket, targets) in [(&self.v4, v4_targets), (&self.v6, v6_targets)] {
            let Some(socket) = socket else { continue };
            for target in targets {
                match socket.send_to(&bytes, target).await {
                    Ok(_) => sent += 1,
                    Err(error) => last_err = Some(error),
                }
            }
        }
        if sent == 0 {
            Err(DiscoveryError::Io(last_err.expect(
                "every fanout send failed, so an error was recorded",
            )))
        } else {
            Ok(sent)
        }
    }

    /// Send one beacon to an explicit address (targeted/unicast discovery,
    /// and the deterministic path tests use). The socket matching the
    /// destination's address family is used.
    pub async fn send_beacon_to(
        &self,
        addr: SocketAddr,
        now_unix: u64,
    ) -> Result<(), DiscoveryError> {
        let socket = match addr {
            SocketAddr::V4(_) => self.v4.as_ref(),
            SocketAddr::V6(_) => self.v6.as_ref(),
        }
        .ok_or_else(|| {
            DiscoveryError::Io(std::io::Error::new(
                std::io::ErrorKind::AddrNotAvailable,
                "no discovery socket is bound for this address family",
            ))
        })?;
        let bytes = self.beacon(now_unix).encode();
        socket.send_to(&bytes, addr).await?;
        Ok(())
    }

    /// Receive one datagram on either socket, validate it as a beacon, and
    /// merge the announcer into the peer table; returns the announced
    /// identity.
    ///
    /// Invalid input surfaces as [`DiscoveryError::Beacon`]; callers in the
    /// spawn loop drop those and keep listening.
    pub async fn recv_beacon(
        &self,
        now_unix: u64,
        now: Instant,
    ) -> Result<PublicKey, DiscoveryError> {
        let mut buf_v4 = [0u8; BEACON_SIZE + 1];
        let mut buf_v6 = [0u8; BEACON_SIZE + 1];
        match (self.v4.as_ref(), self.v6.as_ref()) {
            (Some(v4), Some(v6)) => {
                tokio::select! {
                    result = v4.recv_from(&mut buf_v4) => {
                        let (len, from) = result?;
                        self.accept(&buf_v4[..len], from, now_unix, now)
                    }
                    result = v6.recv_from(&mut buf_v6) => {
                        let (len, from) = result?;
                        self.accept(&buf_v6[..len], from, now_unix, now)
                    }
                }
            }
            (Some(v4), None) => {
                let (len, from) = v4.recv_from(&mut buf_v4).await?;
                self.accept(&buf_v4[..len], from, now_unix, now)
            }
            (None, Some(v6)) => {
                let (len, from) = v6.recv_from(&mut buf_v6).await?;
                self.accept(&buf_v6[..len], from, now_unix, now)
            }
            (None, None) => Err(DiscoveryError::Io(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "no discovery socket is bound",
            ))),
        }
    }

    /// Decode, verify, and merge one received datagram as a beacon. The
    /// source address is the announcer's, used exactly as received: the v6
    /// socket is v6-only, so a source can never be a v4-mapped alias.
    fn accept(
        &self,
        raw: &[u8],
        from: SocketAddr,
        now_unix: u64,
        now: Instant,
    ) -> Result<PublicKey, DiscoveryError> {
        let beacon = Beacon::decode(raw)?;
        beacon.verify(now_unix, self.config.max_age)?;
        self.merge(&beacon, from, now);
        Ok(beacon.public_key)
    }

    /// Merge a validated beacon into the peer table. The peer is addressed
    /// at the datagram's source IP plus the beacon's advertised
    /// [`Beacon::listen_port`]. An address change for a recently seen key is
    /// held back until [`ADDR_REBIND_GRACE`] passes (see module docs).
    fn merge(&self, beacon: &Beacon, from: SocketAddr, now: Instant) {
        // Never let a node discover itself, and ignore beacons that advertise
        // the placeholder port 0 (such a peer is not dialable).
        if beacon.public_key == self.keypair.public || beacon.listen_port == 0 {
            return;
        }
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
