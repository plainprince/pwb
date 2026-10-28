//! The NodeEngine: peer table, lifecycle, messaging, and connection links.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rand::rngs::OsRng;
use rand::RngCore;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use crate::crypto::{DhKeypair, DhPublicKey, Keypair, PublicKey};
use crate::dht::{Contact, NodeId, RoutingTable};
use crate::discovery::{DiscoveryConfig, DiscoveryService, DISCOVERY_PORT};
use crate::stun::discover_mapped_address;
use crate::telemetry::Metrics;

use crate::onion::{CoverConfig, OnionCell};

use super::addr::prefer_family;
use super::e2ee::{self, E2eeState};
use super::envelope::{self, Reassembler, ReplayGuard, MAX_MESSAGE_SIZE};
use super::loops;
use super::messages::{NetworkError, NetworkMessage, OutgoingDatagram, SendFlags};
use super::onion_transport::{
    self, OnionAssembly, OnionKind, OnionRequest, OnionReturnPath, RelayEvent,
};

/// Peers that have been silent for this long are removed from the table.
/// Any authenticated message refreshes a peer, and the heartbeat pings every
/// configured interval, so an entry only expires when the peer is gone.
pub(crate) const PEER_TIMEOUT: Duration = Duration::from_secs(300);

/// How long a bootstrap dial waits for a Pong before giving up.
const BOOTSTRAP_DIAL_TIMEOUT: Duration = Duration::from_millis(1500);

/// Capacity of the inbound authenticated-message queue. A burst beyond this is
/// dropped by the receiver rather than buffered, so the queue can never grow
/// with a slow handler.
const INBOUND_QUEUE: usize = 4096;

/// Capacity of the outbound datagram queue. Datagram transport is lossy by
/// design; a saturated sender drops rather than growing without bound.
const OUTBOUND_QUEUE: usize = 4096;

/// Capacity of each application subscription queue. A subscriber that cannot
/// keep up loses messages (and stays subscribed) instead of pinning memory.
pub(crate) const SUBSCRIBER_QUEUE: usize = 512;

/// Starting envelope sequence for a new engine process.
///
/// Receivers keep a per-sender replay window keyed by sequence number. A
/// process that restarted with sequence 1 would have its first messages
/// rejected as replays of the previous session (the peer's window still holds
/// the earlier high-water mark). Anchoring the start in wall-clock
/// microseconds — with 1024 ids of headroom per microsecond — makes every
/// restart begin above any sequence the previous session could have sent.
fn initial_sequence() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_micros() as u64)
        .unwrap_or(0)
        .saturating_mul(1024)
        .max(1)
}

/// A bootstrap (rendezvous) peer: a well-known node whose address and public
/// key are known in advance so new nodes can dial into the swarm without an
/// out-of-band introduction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapPeer {
    /// The bootstrap node's dialable UDP address.
    pub address: SocketAddr,
    /// The bootstrap node's Ed25519 public key.
    pub public_key: PublicKey,
}

/// Resolve one bootstrap specification into a dialable peer.
///
/// Accepted forms are `<host:port>/<hex_pubkey>` (where `host` may be a DNS
/// name) and the usual `plain://nodes.plain?connect=<ip:port>/<hex_pubkey>`
/// connection link. A DNS name lets an operator publish a stable seed host
/// while its address records change; the public key is still pinned, so
/// resolution cannot substitute a different identity.
///
/// `preferred_family` is this node's own listen address: when the name
/// publishes both `A` and `AAAA`, the candidate matching the family the
/// local socket can actually dial is chosen (an IPv6-only node must not be
/// handed an IPv4 seed address it can never reach), falling back to the
/// other family when nothing matches. `None` keeps the historical
/// IPv4-first choice. A literal address in the spec is returned as written:
/// an operator's explicit target is not second-guessed by family.
///
/// Returns `None` for malformed input or when the host does not resolve. The
/// lookup is bounded so a broken resolver cannot stall startup indefinitely.
pub async fn resolve_bootstrap_peer(
    spec: &str,
    preferred_family: Option<IpAddr>,
) -> Option<BootstrapPeer> {
    let spec = spec.trim();
    if let Some((address, public_key)) = NodeEngine::parse_connection_link(spec) {
        return Some(BootstrapPeer {
            address,
            public_key,
        });
    }

    let (host, pubkey_hex) = spec.rsplit_once('/')?;
    let pubkey_bytes = hex::decode(pubkey_hex.trim()).ok()?;
    if pubkey_bytes.len() != 32 {
        return None;
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&pubkey_bytes);

    let address = if let Ok(address) = host.parse::<SocketAddr>() {
        address
    } else {
        let lookup = tokio::time::timeout(Duration::from_secs(5), tokio::net::lookup_host(host));
        let resolved = lookup.await.ok()?.ok()?;
        prefer_family(resolved, preferred_family)?
    };

    Some(BootstrapPeer {
        address,
        public_key: PublicKey(key),
    })
}

/// Peer information for the peer table.
#[derive(Debug, Clone)]
pub struct PeerInfo {
    pub public_key: PublicKey,
    pub last_seen: Instant,
    pub address: SocketAddr,
    pub reputation: u64,
}

/// Configuration for the NodeEngine.
#[derive(Debug, Clone)]
pub struct NodeConfig {
    pub listen_addr: SocketAddr,
    pub keypair: Keypair,
    /// Well-known bootstrap peers this node dials on startup to join the
    /// swarm. Each entry carries both a dialable address and public key.
    pub bootstrap_peers: Vec<BootstrapPeer>,
    /// Optional STUN server (host:port) used to discover this node's public
    /// IP:port through NAT, so connection links contain a dialable public
    /// address instead of a private LAN address. `None` disables STUN.
    pub stun_server: Option<SocketAddr>,
    pub max_peers: usize,
    pub heartbeat_interval: Duration,
}

/// The core network engine that handles peer-to-peer communication.
pub struct NodeEngine {
    config: NodeConfig,
    peer_table: Arc<Mutex<HashMap<PublicKey, PeerInfo>>>,
    message_sender: mpsc::Sender<(SocketAddr, PublicKey, NetworkMessage)>,
    message_receiver: Mutex<Option<mpsc::Receiver<(SocketAddr, PublicKey, NetworkMessage)>>>,
    outgoing_tx: mpsc::Sender<(SocketAddr, Vec<u8>)>,
    outgoing_rx: Mutex<Option<mpsc::Receiver<OutgoingDatagram>>>,
    socket: Mutex<Option<Arc<UdpSocket>>>,
    /// This node's public IP:port as learned via STUN (NAT traversal). `None`
    /// when STUN is disabled or the query failed; connection links fall back
    /// to the local bound address in that case.
    pub(crate) public_addr: Mutex<Option<SocketAddr>>,
    /// Static X25519 key derived from the Ed25519 identity. Recipients derive
    /// the same public key from the identity key, so no extra handshake is
    /// needed before the first encrypted datagram.
    dh_keypair: DhKeypair,
    /// Monotonic per-sender envelope sequence (shared by clones).
    seq: Arc<AtomicU64>,
    /// Kademlia routing table updated from authenticated peers and
    /// `PeerExchange`/`Nodes` messages. Used by federation for shard routing
    /// and by `FindNode` lookups.
    routing: Arc<Mutex<RoutingTable>>,
    /// Replay window for authenticated inbound messages.
    replay: Arc<Mutex<ReplayGuard>>,
    /// Fragment reassembly state for oversized inbound messages.
    reassembly: Arc<Mutex<Reassembler>>,
    /// Fragment reassembly for onion-delivered messages (separate from the
    /// envelope reassembler because exits emit plain cell payloads).
    onion_assembly: Arc<Mutex<OnionAssembly>>,
    /// Application listeners for onion-delivered messages. The listener sees
    /// the authenticated originator key but no dialable address: the onion
    /// path intentionally hides it.
    onion_subscribers: loops::OnionSubscribers,
    /// Listeners for onion requests that carry an answer circuit.
    onion_request_subscribers: loops::OnionRequestSubscribers,
    /// In-flight request/reply circuits, keyed by request id.
    pending_onion_replies: loops::PendingOnionReplies,
    /// Listeners for relayed onion cells (relay-credit accounting).
    relay_subscribers: loops::RelaySubscribers,
    pub(super) running: Arc<Mutex<bool>>,
    discovery_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Address the LAN discovery socket actually bound to (`None` when
    /// discovery is disabled for loopback-only nodes).
    discovery_addr: Mutex<Option<SocketAddr>>,
    /// Application-level listeners for inbound messages. The engine still runs
    /// its own peer-table maintenance on every message; each inbound message is
    /// additionally forwarded to every subscriber so higher layers (e.g. the
    /// federated search service) can answer protocol requests.
    subscribers: loops::Subscribers,
    /// Process-wide counters exposed by the node's health endpoint.
    metrics: Arc<Metrics>,
    /// Peers reached through a relay: target key -> relay key. Direct traffic
    /// clears the route, so a node that becomes dialable again stops paying
    /// the relay hop.
    relay_routes: Arc<Mutex<HashMap<PublicKey, PublicKey>>>,
    /// In-flight `hole_punch` callers waiting for the target's probe to reveal
    /// its NAT-mapped address.
    punch_waiters: Arc<Mutex<HashMap<PublicKey, mpsc::Sender<SocketAddr>>>>,
    /// Opt-in decoy dispatch. `None` (the default) sends no cover traffic;
    /// enabling it costs bandwidth and is a local privacy decision.
    cover: Mutex<Option<CoverConfig>>,
    /// Anti-replay state for inbound [`NetworkMessage::Sealed`] payloads
    /// opened with [`SendFlags::E2EE`].
    e2ee: Arc<E2eeState>,
}

impl NodeEngine {
    /// Create a new NodeEngine with the given configuration.
    pub fn new(config: NodeConfig) -> Self {
        let (tx, rx) = mpsc::channel(INBOUND_QUEUE);
        let (outgoing_tx, outgoing_rx) = mpsc::channel(OUTBOUND_QUEUE);
        let dh_keypair = DhKeypair::from_ed25519(&config.keypair);
        let local_id = NodeId::from_public_key(&config.keypair.public);

        Self {
            config,
            peer_table: Arc::new(Mutex::new(HashMap::new())),
            message_sender: tx,
            message_receiver: Mutex::new(Some(rx)),
            outgoing_tx,
            outgoing_rx: Mutex::new(Some(outgoing_rx)),
            socket: Mutex::new(None),
            public_addr: Mutex::new(None),
            dh_keypair,
            routing: Arc::new(Mutex::new(RoutingTable::new(local_id))),
            seq: Arc::new(AtomicU64::new(initial_sequence())),
            replay: Arc::new(Mutex::new(ReplayGuard::default())),
            reassembly: Arc::new(Mutex::new(Reassembler::default())),
            onion_assembly: Arc::new(Mutex::new(OnionAssembly::default())),
            onion_subscribers: Arc::new(Mutex::new(Vec::new())),
            onion_request_subscribers: Arc::new(Mutex::new(Vec::new())),
            pending_onion_replies: Arc::new(Mutex::new(HashMap::new())),
            relay_subscribers: Arc::new(Mutex::new(Vec::new())),
            running: Arc::new(Mutex::new(false)),
            discovery_handle: Mutex::new(None),
            discovery_addr: Mutex::new(None),
            subscribers: Arc::new(Mutex::new(Vec::new())),
            metrics: Arc::new(Metrics::new()),
            relay_routes: Arc::new(Mutex::new(HashMap::new())),
            punch_waiters: Arc::new(Mutex::new(HashMap::new())),
            cover: Mutex::new(None),
            e2ee: Arc::new(E2eeState::default()),
        }
    }

    /// Enable decoy onion traffic with the given timing configuration.
    ///
    /// Call this before [`Self::start`]; the dispatcher is spawned with the
    /// other engine loops. Cover cells are ordinary fixed-size onion cells
    /// with random payloads over randomized paths, so a relay cannot tell
    /// them from a real request. The default is disabled: cover traffic costs
    /// bandwidth and only helps where it actually runs.
    pub fn enable_cover_traffic(&self, config: CoverConfig) {
        *self.cover.lock().unwrap() = Some(config);
    }

    /// The active cover-traffic configuration, if enabled.
    pub fn cover_traffic(&self) -> Option<CoverConfig> {
        self.cover.lock().unwrap().clone()
    }

    /// The engine's process-wide counters, shared with the node's health
    /// endpoint. Cheap to clone; all counters are atomic.
    pub fn metrics(&self) -> Arc<Metrics> {
        Arc::clone(&self.metrics)
    }

    /// Start the network engine (spawns background tasks).
    ///
    /// Binds the UDP listen socket, then spawns the receiver loop (verifies
    /// signed envelopes, reassembles fragments, drops replays), the message
    /// handler loop, the outgoing sender loop, the peer maintenance/liveness
    /// loop, and — for non-loopback nodes — a [`DiscoveryService`] that shares
    /// this engine's peer table so LAN beacons immediately populate it.
    ///
    /// Starting is idempotent. Bootstrap peers are *not* dialed here; call
    /// [`Self::connect_bootstrap_verified`] (or [`Self::dial`]) to join, and
    /// read [`Self::discovery_addr`] to see where beacons are bound.
    pub async fn start(&self) -> Result<(), NetworkError> {
        if self.is_running() {
            return Ok(());
        }

        // Bind the UDP listen socket for all network I/O. The engine only
        // becomes "running" once the socket exists, so a failed bind leaves
        // it startable instead of permanently half-started.
        let socket = Arc::new(bind_udp_socket(self.config.listen_addr)?);
        let bound_addr = socket.local_addr()?;
        *self.socket.lock().unwrap() = Some(Arc::clone(&socket));
        self.set_running();

        // NAT traversal: ask the STUN server for our public IP:port so
        // connection links advertise a dialable public address instead of a
        // private LAN address. Best-effort: a failed STUN query falls back to
        // the local bound address.
        if let Some(stun_server) = self.config.stun_server {
            if let Some(mapped) =
                discover_mapped_address(&socket, stun_server, Duration::from_secs(5)).await
            {
                *self.public_addr.lock().unwrap() = Some(mapped);
            }
        }

        // Take the receivers for the background loops. A missing receiver
        // means a concurrent start won the race; undo the half-start instead
        // of panicking.
        let Some(message_receiver) = self.message_receiver.lock().unwrap().take() else {
            *self.running.lock().unwrap() = false;
            return Err(NetworkError::InvalidMessage);
        };
        let Some(outgoing_rx) = self.outgoing_rx.lock().unwrap().take() else {
            *self.running.lock().unwrap() = false;
            return Err(NetworkError::InvalidMessage);
        };

        let peer_table = Arc::clone(&self.peer_table);
        let peer_table_for_receiver = Arc::clone(&self.peer_table);
        let peer_table_for_maintenance = Arc::clone(&self.peer_table);
        let routing = Arc::clone(&self.routing);
        let routing_for_receiver = Arc::clone(&routing);
        let routing_for_handler = Arc::clone(&routing);
        let config = self.config.clone();
        let config_for_maintenance = self.config.clone();
        let running_clone = Arc::clone(&self.running);
        let running_clone2 = Arc::clone(&self.running);
        let running_clone3 = Arc::clone(&self.running);
        let running_clone4 = Arc::clone(&self.running);
        let running_clone5 = Arc::clone(&self.running);
        let message_sender = self.message_sender.clone();
        let subscribers = Arc::clone(&self.subscribers);
        let outgoing_for_handler = self.outgoing_tx.clone();
        let outgoing_for_heartbeat = self.outgoing_tx.clone();
        let seq_for_handler = Arc::clone(&self.seq);
        let seq_for_heartbeat = Arc::clone(&self.seq);
        let replay = Arc::clone(&self.replay);
        let reassembly = Arc::clone(&self.reassembly);
        let onion_assembly = Arc::clone(&self.onion_assembly);
        let onion_subscribers = Arc::clone(&self.onion_subscribers);
        let onion_request_subscribers = Arc::clone(&self.onion_request_subscribers);
        let pending_onion_replies = Arc::clone(&self.pending_onion_replies);
        let relay_subscribers = Arc::clone(&self.relay_subscribers);
        let onion_outgoing = self.outgoing_tx.clone();
        let local_dh = self.dh_keypair.clone();
        let metrics_for_sender = Arc::clone(&self.metrics);
        let metrics_for_receiver = Arc::clone(&self.metrics);
        let relay_routes_for_receiver = Arc::clone(&self.relay_routes);
        let punch_waiters_for_receiver = Arc::clone(&self.punch_waiters);
        let relay_routes_for_handler = Arc::clone(&self.relay_routes);
        let advertised_addr = self.advertised_addr().unwrap_or(bound_addr);
        let e2ee_for_receiver = Arc::clone(&self.e2ee);

        // Spawn message handler loop (processes inbound messages and forwards
        // them to application subscribers).
        tokio::spawn(async move {
            loops::message_handler_loop(
                message_receiver,
                loops::HandlerState {
                    peer_table,
                    config,
                    running: running_clone,
                    subscribers,
                    outgoing_tx: outgoing_for_handler,
                    seq: seq_for_handler,
                    routing: routing_for_handler,
                    relay_routes: relay_routes_for_handler,
                    advertised_addr,
                },
            )
            .await;
        });
        // Spawn outgoing sender loop (transmits queued datagrams to peers).
        tokio::spawn(async move {
            loops::sender_loop(socket, outgoing_rx, running_clone2, metrics_for_sender).await;
        });

        // Optional decoy dispatch. Kept off unless the operator asked for it;
        // the loop samples an exponential delay and needs at least three peer
        // relays to form a path.
        if let Some(cover_config) = self.cover.lock().unwrap().clone() {
            let cover_outgoing = self.outgoing_tx.clone();
            let cover_peers = Arc::clone(&self.peer_table);
            let cover_metrics = Arc::clone(&self.metrics);
            tokio::spawn(async move {
                loops::cover_traffic_loop(
                    cover_outgoing,
                    cover_peers,
                    running_clone5,
                    cover_metrics,
                    cover_config,
                )
                .await;
            });
        }

        // Spawn inbound receiver loop (verifies envelopes, reassembles
        // fragments, drops replays, then dispatches messages).
        let receiver_socket = self.socket.lock().unwrap().as_ref().unwrap().clone();
        let local_key = self.config.keypair.public;
        let local_keypair = self.config.keypair.clone();
        let max_peers = self.config.max_peers;
        tokio::spawn(async move {
            loops::receiver_loop(
                receiver_socket,
                message_sender,
                running_clone3,
                loops::InboundState {
                    peer_table: peer_table_for_receiver,
                    max_peers,
                    replay: Arc::clone(&replay),
                    reassembly: Arc::clone(&reassembly),
                    onion_replay: Mutex::new(loops::OnionReplayCache::default()),
                    onion_assembly,
                    onion_subscribers,
                    onion_request_subscribers,
                    pending_onion_replies,
                    relay_subscribers,
                    outgoing_tx: onion_outgoing,
                    routing: routing_for_receiver,
                    local_key,
                    local_keypair,
                    local_dh,
                    e2ee: e2ee_for_receiver,
                    metrics: metrics_for_receiver,
                    relay_routes: relay_routes_for_receiver,
                    punch_waiters: punch_waiters_for_receiver,
                },
            )
            .await;
        });

        // Spawn peer maintenance loop: prune dead peers and ping live ones so
        // `last_seen` reflects actual reachability.
        tokio::spawn(async move {
            loops::peer_maintenance_loop(
                peer_table_for_maintenance,
                config_for_maintenance,
                running_clone4,
                outgoing_for_heartbeat,
                seq_for_heartbeat,
            )
            .await;
        });

        // Start LAN discovery sharing this engine's peer table. Loopback-only
        // nodes skip discovery: beacons cannot reach them, and binding the
        // shared discovery port would collide with other local test nodes.
        if !self.config.listen_addr.ip().is_loopback() {
            let listen_port = bound_addr.port();
            let discovery = match DiscoveryService::bind_with_table(
                DiscoveryConfig {
                    listen_port,
                    bind_port: DISCOVERY_PORT,
                    beacon_interval: self.config.heartbeat_interval,
                    max_age: crate::discovery::BEACON_MAX_AGE,
                },
                self.config.keypair.clone(),
                Arc::clone(&self.peer_table),
            )
            .await
            {
                Ok(service) => Some(service),
                // Another node on this host already holds the discovery port;
                // fall back to an ephemeral one so we still announce.
                Err(_) => DiscoveryService::bind_with_table(
                    DiscoveryConfig {
                        listen_port,
                        bind_port: 0,
                        beacon_interval: self.config.heartbeat_interval,
                        max_age: crate::discovery::BEACON_MAX_AGE,
                    },
                    self.config.keypair.clone(),
                    Arc::clone(&self.peer_table),
                )
                .await
                .ok(),
            };
            if let Some(discovery) = discovery {
                if let Ok(addr) = discovery.local_addr() {
                    *self.discovery_addr.lock().unwrap() = Some(addr);
                }
                let handle = discovery.spawn();
                *self.discovery_handle.lock().unwrap() = Some(handle);
            }
        }

        Ok(())
    }

    /// Stop the network engine.
    pub async fn stop(&self) {
        *self.running.lock().unwrap() = false;
        // Abort the discovery background task.
        if let Some(handle) = self.discovery_handle.lock().unwrap().take() {
            handle.abort();
        }
    }

    /// Whether the engine has been started.
    fn is_running(&self) -> bool {
        *self.running.lock().unwrap()
    }

    /// Mark the engine as running.
    fn set_running(&self) {
        *self.running.lock().unwrap() = true;
    }

    /// This engine's identity public key (the key that signs every envelope).
    pub fn public_key(&self) -> PublicKey {
        self.config.keypair.public
    }

    /// This engine's bound UDP socket address (the address peers observe as
    /// the source of this node's datagrams). Returns `None` before `start()`
    /// has bound the socket.
    pub fn local_addr(&self) -> Option<std::io::Result<SocketAddr>> {
        self.socket.lock().unwrap().as_ref().map(|s| s.local_addr())
    }

    /// Encode `message` into signed, encrypted, fragmented datagrams for one
    /// recipient identity.
    fn encode_message_for(
        &self,
        recipient: &PublicKey,
        message: &NetworkMessage,
    ) -> Result<Vec<Vec<u8>>, NetworkError> {
        let bytes = serde_json::to_vec(message).map_err(NetworkError::Serialization)?;
        if bytes.len() > MAX_MESSAGE_SIZE {
            return Err(NetworkError::MessageTooLarge {
                len: bytes.len(),
                max: MAX_MESSAGE_SIZE,
            });
        }
        let recipient_dh = DhPublicKey::from_ed25519(recipient).ok_or_else(|| {
            NetworkError::InvalidConnectionLink(format!(
                "peer key {} is not a valid curve point",
                recipient.to_hex()
            ))
        })?;
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        envelope::encode_for(
            &self.config.keypair,
            &recipient_dh,
            seq,
            crate::discovery::unix_time(),
            &bytes,
        )
        .map_err(|_| NetworkError::MessageTooLarge {
            len: bytes.len(),
            max: MAX_MESSAGE_SIZE,
        })
    }

    /// Send a message to a specific peer.
    ///
    /// Serializes the [`NetworkMessage`] into signed, encrypted envelope
    /// datagrams and transmits them to the peer's registered UDP address. Delivery is
    /// strictly remote: the engine never re-injects its own outbound message
    /// into its local handler (doing so previously registered the sender's own
    /// key at the peer's address). Returns [`NetworkError::PeerNotFound`] when
    /// the peer is not in the peer table.
    pub fn send_to(&self, peer: &PublicKey, message: NetworkMessage) -> Result<(), NetworkError> {
        self.send_to_with(peer, message, SendFlags::EMPTY)
    }

    /// [`Self::send_to`] with send flags. Passing [`SendFlags::E2EE`] seals
    /// `message` for the peer before it is serialized (see
    /// `pwb-core::net::e2ee`); the receiver opens it and delivers the inner
    /// message to its subscribers as if it had arrived unsealed.
    pub fn send_to_with(
        &self,
        peer: &PublicKey,
        message: NetworkMessage,
        flags: SendFlags,
    ) -> Result<(), NetworkError> {
        let key = *peer;
        // Resolve the peer first so an unknown peer reports PeerNotFound
        // before any per-message crypto work.
        let (addr, via) = {
            let peer_table = self.peer_table.lock().unwrap();
            let Some(info) = peer_table.get(&key) else {
                return Err(NetworkError::PeerNotFound(key.to_hex()));
            };
            let via = self.relay_routes.lock().unwrap().get(&key).copied();
            (info.address, via)
        };

        let message = if flags.contains(SendFlags::E2EE) {
            self.seal_message(peer, message)?
        } else {
            message
        };
        let datagrams = self.encode_message_for(peer, &message)?;
        match via {
            Some(relay) => self.send_relayed(&relay, peer, addr, datagrams),
            None => {
                for datagram in datagrams {
                    // UDP semantics: a full send queue drops this datagram.
                    let _ = self.outgoing_tx.try_send((addr, datagram));
                }
                Ok(())
            }
        }
    }

    /// Wrap already-encoded envelopes for `target` in a [`RelayDeliver`]
    /// message and hand them to `relay`. The relay never sees plaintext: the
    /// inner datagrams stay encrypted and signed for the target.
    fn send_relayed(
        &self,
        relay: &PublicKey,
        target: &PublicKey,
        _target_addr: SocketAddr,
        datagrams: Vec<Vec<u8>>,
    ) -> Result<(), NetworkError> {
        let relay_addr = {
            let peer_table = self.peer_table.lock().unwrap();
            let relay_peer = peer_table
                .get(relay)
                .ok_or_else(|| NetworkError::PeerNotFound(relay.to_hex()))?;
            // Only one relay hop: a relay that is itself relayed would need a
            // routing loop detector, and a direct path always exists for the
            // relay we chose.
            if self.relay_routes.lock().unwrap().contains_key(relay) {
                return Err(NetworkError::PeerNotFound(relay.to_hex()));
            }
            relay_peer.address
        };
        let origin_addr = self.advertised_addr().ok_or(NetworkError::InvalidMessage)?;
        let wrapper = NetworkMessage::RelayDeliver {
            origin: self.config.keypair.public,
            origin_addr,
            target: *target,
            datagrams,
        };
        self.metrics.record_relay_sent();
        for datagram in self.encode_message_for(relay, &wrapper)? {
            let _ = self.outgoing_tx.try_send((relay_addr, datagram));
        }
        Ok(())
    }

    /// Seal `message` for `recipient` as a [`NetworkMessage::Sealed`] wrapper.
    ///
    /// This is the composable E2EE primitive: use it directly when the flag
    /// form does not apply, e.g.
    /// `engine.send_onion_to(addr, &path, &engine.seal_message(&target, msg)?)`
    /// to carry an end-to-end-encrypted payload across an onion circuit.
    pub fn seal_message(
        &self,
        recipient: &PublicKey,
        message: NetworkMessage,
    ) -> Result<NetworkMessage, NetworkError> {
        e2ee::seal(&self.config.keypair.public, recipient, &message)
    }

    /// Broadcast a message to all connected peers.
    ///
    /// Encodes one copy per recipient (each with its own ephemeral key), then
    /// transmits the signed ciphertext to every peer's registered UDP address.
    /// Like [`Self::send_to`], nothing is delivered
    /// locally: the sender already knows the message it just sent.
    pub fn broadcast(&self, message: NetworkMessage) -> Result<(), NetworkError> {
        self.broadcast_with(message, SendFlags::EMPTY)
    }

    /// [`Self::broadcast`] with send flags (e.g. [`SendFlags::E2EE`]). Each
    /// recipient gets its own sealed ciphertext.
    pub fn broadcast_with(
        &self,
        message: NetworkMessage,
        flags: SendFlags,
    ) -> Result<(), NetworkError> {
        let peers = {
            let peer_table = self.peer_table.lock().unwrap();
            peer_table.keys().copied().collect::<Vec<_>>()
        };
        for public_key in peers {
            // Each peer has its own ciphertext, and relayed peers go through
            // their relay rather than being sent a direct datagram they cannot
            // receive.
            self.send_to_with(&public_key, message.clone(), flags)?;
        }
        Ok(())
    }

    /// Send a message straight to a UDP address, bypassing the peer table.
    ///
    /// Used by application handlers that receive an inbound datagram (and thus
    /// the sender's real address) but were not given a public key to key a
    /// `send_to` reply against. The peer must still be in the peer table: the
    /// payload is encrypted for the authenticated key at that address, and an
    /// unknown address is refused rather than sent in plaintext.
    pub fn send_to_addr(
        &self,
        addr: SocketAddr,
        message: NetworkMessage,
    ) -> Result<(), NetworkError> {
        // Replies are encrypted for the peer at this address. The receiver
        // loop registers every authenticated sender before dispatch, so the
        // normal reply path always has a key. Sending to an address we have
        // never authenticated is refused rather than falling back to
        // plaintext.
        let public_key = {
            let peer_table = self.peer_table.lock().unwrap();
            peer_table
                .values()
                .find(|peer| peer.address == addr)
                .map(|peer| peer.public_key)
        }
        .ok_or_else(|| NetworkError::PeerNotFound(addr.to_string()))?;
        // Delegate so a relayed peer's reply follows the relay route instead
        // of being sent directly to an address behind a NAT.
        self.send_to(&public_key, message)
    }

    /// Subscribe to every inbound [`NetworkMessage`] the engine receives,
    /// tagged with the datagram's real source address. The engine keeps its own
    /// peer-maintenance behavior; subscribers are purely additive. The queue is
    /// bounded (512 messages): a subscriber that falls behind loses messages
    /// instead of growing without bound, and closed subscribers are pruned
    /// automatically.
    pub fn subscribe(&self) -> mpsc::Receiver<(SocketAddr, NetworkMessage)> {
        let (tx, rx) = mpsc::channel(SUBSCRIBER_QUEUE);
        self.subscribers.lock().unwrap().push(tx);
        rx
    }

    /// Subscribe to messages delivered by an onion exit. The tuple carries the
    /// message's signed originator key; there is deliberately no source address
    /// (the onion path hides it). The queue is bounded like [`Self::subscribe`].
    pub fn subscribe_onion(&self) -> mpsc::Receiver<(PublicKey, NetworkMessage)> {
        let (tx, rx) = mpsc::channel(SUBSCRIBER_QUEUE);
        self.onion_subscribers.lock().unwrap().push(tx);
        rx
    }

    /// Subscribe to onion requests, including the return path needed to
    /// answer them with [`Self::send_onion_reply`].
    pub fn subscribe_onion_requests(&self) -> mpsc::Receiver<OnionRequest> {
        let (tx, rx) = mpsc::channel(SUBSCRIBER_QUEUE);
        self.onion_request_subscribers.lock().unwrap().push(tx);
        rx
    }

    /// Subscribe to relayed onion cells so the node layer can meter relay work
    /// and mint relay credits.
    pub fn subscribe_relay(&self) -> mpsc::Receiver<RelayEvent> {
        let (tx, rx) = mpsc::channel(SUBSCRIBER_QUEUE);
        self.relay_subscribers.lock().unwrap().push(tx);
        rx
    }

    /// The local X25519 relay key (the last hop of an onion path addressed to
    /// this node, and what a local onion relay peels with).
    pub fn onion_key(&self) -> DhPublicKey {
        self.dh_keypair.public
    }

    /// Resolve a relay's long-term onion key to a dialable address by matching
    /// it against the authenticated peer table.
    pub fn onion_address(&self, relay: &DhPublicKey) -> Option<SocketAddr> {
        let table = self.peer_table.lock().unwrap();
        table
            .keys()
            .find(|key| DhPublicKey::from_ed25519(key) == Some(*relay))
            .and_then(|key| table.get(key))
            .map(|peer| peer.address)
    }

    /// Encapsulate `message` in an onion path and send it through the entry
    /// relay.
    ///
    /// The path is entry-first/exit-last and must satisfy
    /// [`crate::onion::validate_path`] (3..=6 unique relays). The message is
    /// chunked across as many cells as needed because one cell carries only
    /// [`crate::onion::PAYLOAD_MAX`] payload bytes.
    ///
    /// This is a one-way request: no return path is attached, so the exit
    /// cannot answer. Use [`Self::send_onion_request`] for request/reply.
    pub fn send_onion(
        &self,
        path: &[DhPublicKey],
        message: &NetworkMessage,
    ) -> Result<(), NetworkError> {
        let first = *path.first().ok_or(NetworkError::InvalidMessage)?;
        let addr = self
            .onion_address(&first)
            .ok_or_else(|| NetworkError::PeerNotFound(first.to_hex()))?;
        self.send_onion_to(addr, path, message)
    }

    /// [`Self::send_onion`] with an explicit entry-hop address (used when a
    /// caller already knows the first relay's dial address, e.g. a connection
    /// link or a test).
    pub fn send_onion_to(
        &self,
        first_hop: SocketAddr,
        path: &[DhPublicKey],
        message: &NetworkMessage,
    ) -> Result<(), NetworkError> {
        let mut request_id = [0u8; 16];
        OsRng.fill_bytes(&mut request_id);
        self.send_onion_framed(
            &self.config.keypair,
            first_hop,
            path,
            OnionKind::Request,
            request_id,
            None,
            message,
        )
    }

    /// Send a request through an onion circuit and wait for the exit's answer.
    ///
    /// The request carries a return path built from the second-to-last relay
    /// of `path` back through the earlier relays to this node, so the exit can
    /// reply without ever learning this node's address. Every request is
    /// retransmitted (with fresh cells) a few times within `timeout`, because
    /// UDP can drop any of the multi-cell fragments. A retransmission can
    /// therefore be delivered more than once; requests should be idempotent
    /// (reads, lookups) or carry their own application-level dedup.
    pub async fn send_onion_request(
        &self,
        path: &[DhPublicKey],
        message: &NetworkMessage,
        timeout: Duration,
    ) -> Result<NetworkMessage, NetworkError> {
        self.send_onion_request_as(&self.config.keypair, path, message, timeout)
            .await
    }

    /// Like [`Self::send_onion_request`], but signs the onion envelope with
    /// `signing_key` instead of the node's long-term identity. Pass a fresh
    /// ephemeral key when the exit must not be able to link the request to this
    /// node; any application field that the exit compares against the onion
    /// originator (for example `ShardQuery.sender`) must use the same key.
    pub async fn send_onion_request_as(
        &self,
        signing_key: &Keypair,
        path: &[DhPublicKey],
        message: &NetworkMessage,
        timeout: Duration,
    ) -> Result<NetworkMessage, NetworkError> {
        if path.len() < 3 {
            return Err(NetworkError::InvalidMessage);
        }
        let entry = path[0];
        let entry_addr = self
            .onion_address(&entry)
            .ok_or_else(|| NetworkError::PeerNotFound(entry.to_hex()))?;
        let return_path = self.build_return_path(path)?;

        let mut request_id = [0u8; 16];
        OsRng.fill_bytes(&mut request_id);
        let (tx, mut rx) = mpsc::channel(1);
        self.pending_onion_replies
            .lock()
            .unwrap()
            .insert(request_id, tx);

        let deadline = tokio::time::Instant::now() + timeout;
        let retry = (timeout / 3).max(Duration::from_millis(120));
        let result = loop {
            self.send_onion_framed(
                signing_key,
                entry_addr,
                path,
                OnionKind::Request,
                request_id,
                Some(&return_path),
                message,
            )?;
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break Err(NetworkError::Timeout);
            }
            match tokio::time::timeout(remaining.min(retry), rx.recv()).await {
                Ok(Some(reply)) => break Ok(reply),
                Ok(None) => break Err(NetworkError::Timeout),
                Err(_) => {
                    if tokio::time::Instant::now() >= deadline {
                        break Err(NetworkError::Timeout);
                    }
                }
            }
        };
        self.pending_onion_replies
            .lock()
            .unwrap()
            .remove(&request_id);
        result
    }

    /// Answer an onion request through its return path. The reply obeys the
    /// same size limit as a request (48 KiB) because it is chunked across
    /// cells as well.
    pub fn send_onion_reply(
        &self,
        request: &OnionRequest,
        message: &NetworkMessage,
    ) -> Result<(), NetworkError> {
        let return_path = request
            .return_path
            .as_ref()
            .filter(|path| path.is_valid())
            .ok_or(NetworkError::InvalidMessage)?;
        self.send_onion_framed(
            &self.config.keypair,
            return_path.first_hop,
            &return_path.path,
            OnionKind::Reply,
            request.request_id,
            None,
            message,
        )
    }

    /// Build the return leg of a circuit: the outbound path without its exit
    /// (which is the responder), traversed backwards, then this node's own
    /// onion key as the final destination.
    fn build_return_path(&self, path: &[DhPublicKey]) -> Result<OnionReturnPath, NetworkError> {
        if path.len() < 3 {
            return Err(NetworkError::InvalidMessage);
        }
        let entry = path[path.len() - 2];
        let first_hop = self
            .onion_address(&entry)
            .ok_or_else(|| NetworkError::PeerNotFound(entry.to_hex()))?;
        let mut return_hops: Vec<DhPublicKey> =
            path[..path.len() - 1].iter().rev().copied().collect();
        return_hops.push(self.dh_keypair.public);
        if crate::onion::validate_path(&return_hops).is_err() {
            return Err(NetworkError::InvalidMessage);
        }
        Ok(OnionReturnPath {
            first_hop,
            path: return_hops,
        })
    }

    /// Frame `message` as an onion request or reply and emit one cell per
    /// chunk through `first_hop`.
    #[allow(clippy::too_many_arguments)]
    fn send_onion_framed(
        &self,
        signing_key: &Keypair,
        first_hop: SocketAddr,
        path: &[DhPublicKey],
        kind: OnionKind,
        request_id: [u8; 16],
        return_path: Option<&OnionReturnPath>,
        message: &NetworkMessage,
    ) -> Result<(), NetworkError> {
        let bytes = serde_json::to_vec(message).map_err(NetworkError::Serialization)?;
        let chunks = onion_transport::encode_message_with(
            signing_key,
            kind,
            request_id,
            return_path,
            &bytes,
        )
        .ok_or(NetworkError::MessageTooLarge {
            len: bytes.len(),
            max: 48 * 1024,
        })?;
        for chunk in chunks {
            let cell = OnionCell::encapsulate(&chunk, path)?;
            let _ = self
                .outgoing_tx
                .try_send((first_hop, cell.to_bytes().to_vec()));
        }
        Ok(())
    }

    /// Get a reference to the peer table (read-only).
    pub fn peer_table(&self) -> Arc<Mutex<HashMap<PublicKey, PeerInfo>>> {
        Arc::clone(&self.peer_table)
    }

    /// The live Kademlia routing table populated from authenticated peers,
    /// `PeerExchange`, and `Nodes` replies.
    pub fn routing_table(&self) -> Arc<Mutex<RoutingTable>> {
        Arc::clone(&self.routing)
    }

    /// The `count` known contacts closest (XOR metric) to `target`. This is the
    /// live DHT view used for shard placement and lookups; it excludes the
    /// local node (callers add themselves when they may be responsible).
    pub fn closest_contacts(&self, target: &NodeId, count: usize) -> Vec<Contact> {
        self.routing.lock().unwrap().closest(target, count)
    }

    /// Iterative Kademlia `FIND_NODE` lookup for `target`.
    ///
    /// Starts from the contacts this node already knows (routing table plus
    /// the flat peer table), then repeatedly asks the `ALPHA` closest
    /// unqueried candidates for *their* closest contacts. Replies are merged
    /// into the candidate set and into the routing table (the message handler
    /// does the latter), and the walk stops once the `K` closest known
    /// contacts have all answered, no new candidates appear, or `timeout`
    /// expires.
    ///
    /// Returns the `K` closest contacts discovered, nearest first.
    pub async fn find_node(&self, target: NodeId, timeout: Duration) -> Vec<Contact> {
        const ALPHA: usize = 3;
        const K: usize = 20;
        /// How long one round waits for `Nodes` replies before moving on.
        const ROUND: Duration = Duration::from_millis(350);

        let local = self.config.keypair.public;
        let mut known: HashMap<NodeId, Contact> = HashMap::new();
        for contact in self.routing.lock().unwrap().iter() {
            known.insert(contact.node_id, contact.clone());
        }
        for (key, peer) in self.peer_table.lock().unwrap().iter() {
            if *key == local {
                continue;
            }
            let id = NodeId::from_public_key(key);
            known
                .entry(id)
                .or_insert_with(|| Contact::new(*key, peer.address));
        }
        if known.is_empty() {
            return Vec::new();
        }

        let mut queried: HashSet<NodeId> = HashSet::new();
        let mut replies = self.subscribe();
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            let mut candidates: Vec<Contact> = known
                .iter()
                .filter(|(id, _)| !queried.contains(id))
                .map(|(_, contact)| contact.clone())
                .collect();
            candidates.sort_by_key(|contact| contact.node_id.xor(&target));
            candidates.truncate(ALPHA);
            if candidates.is_empty() {
                break;
            }

            let mut sent = false;
            for contact in &candidates {
                queried.insert(contact.node_id);
                if contact.public_key == local {
                    continue;
                }
                if self
                    .send_to(
                        &contact.public_key,
                        NetworkMessage::FindNode {
                            target: *target.as_bytes(),
                            sender: local,
                        },
                    )
                    .is_ok()
                {
                    sent = true;
                }
            }
            if !sent {
                continue;
            }

            // Collect this round's answers. Each `Nodes` reply adds contacts
            // that sort ahead of the current frontier if they are closer.
            let round_deadline = (tokio::time::Instant::now() + ROUND).min(deadline);
            loop {
                match tokio::time::timeout_at(round_deadline, replies.recv()).await {
                    Ok(Some((_, NetworkMessage::Nodes { peers, .. }))) => {
                        for (address, public_key) in peers {
                            if public_key == local || address.port() == 0 {
                                continue;
                            }
                            let id = NodeId::from_public_key(&public_key);
                            known
                                .entry(id)
                                .or_insert_with(|| Contact::new(public_key, address));
                        }
                    }
                    Ok(Some(_)) => continue,
                    Ok(None) | Err(_) => break,
                }
            }

            // Converged once every one of the K closest known contacts has
            // been asked and none of them produced a closer unseen contact.
            let mut closest: Vec<&Contact> = known.values().collect();
            closest.sort_by_key(|contact| contact.node_id.xor(&target));
            if closest
                .iter()
                .take(K)
                .all(|contact| queried.contains(&contact.node_id))
            {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
        }

        let mut contacts: Vec<Contact> = known.into_values().collect();
        contacts.sort_by_key(|contact| contact.node_id.xor(&target));
        contacts.truncate(K);
        contacts
    }

    /// Add a peer to the peer table.
    pub fn add_peer(&self, peer_info: PeerInfo) {
        let mut peer_table = self.peer_table.lock().unwrap();
        peer_table.insert(peer_info.public_key, peer_info);
    }

    /// Remove a peer from the peer table.
    pub fn remove_peer(&self, public_key: &PublicKey) {
        let mut peer_table = self.peer_table.lock().unwrap();
        peer_table.remove(public_key);
        drop(peer_table);
        let id = NodeId::from_public_key(public_key);
        self.routing.lock().unwrap().remove(&id);
    }

    /// Get a peer from the peer table.
    pub fn get_peer(&self, public_key: &PublicKey) -> Option<PeerInfo> {
        let peer_table = self.peer_table.lock().unwrap();
        peer_table.get(public_key).cloned()
    }

    /// Generate a connection link URI for this node that can be scanned/clicked to connect.
    ///
    /// The link is "double-sided" - it contains our public key so any network
    /// that scans it will peer with us, and when two such networks merge,
    /// peers from both sides are recognized and connected.
    ///
    /// Uses the STUN-discovered public address (NAT traversal) when available,
    /// otherwise the *actual* bound socket address when the engine has been
    /// started; falls back to [`NodeConfig::listen_addr`] before `start()`.
    ///
    /// Format: `plain://nodes.plain?connect=<ip:port>/<pubkey_hex>`
    pub fn generate_connection_link(&self) -> String {
        let public = *self.public_addr.lock().unwrap();
        let addr = public
            .or_else(|| {
                self.local_addr()
                    .and_then(|r| r.ok())
                    .map(|addr| self.dialable_address(addr))
            })
            .unwrap_or(self.config.listen_addr);
        Self::build_connection_link(addr, self.config.keypair.public)
    }

    /// Return a dialable address for `bound_addr`. If the node bound to an
    /// unspecified address (`0.0.0.0` / `::`), STUN may still have learned a
    /// public address; otherwise we probe the OS routing table to find a
    /// routable LAN IP for this node's port so the link is dialable without
    /// STUN.
    fn dialable_address(&self, bound_addr: SocketAddr) -> SocketAddr {
        if !bound_addr.ip().is_unspecified() {
            return bound_addr;
        }
        if let Some(ip) = best_effort_local_ip(bound_addr) {
            return SocketAddr::new(ip, bound_addr.port());
        }
        // A dual-stack `[::]` socket on a host without IPv6 routing has no
        // v6 source address, but it is still reachable over IPv4: probe that
        // family too instead of advertising the unspecified address.
        if bound_addr.is_ipv6() {
            let v4_probe = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), bound_addr.port());
            if let Some(ip) = best_effort_local_ip(v4_probe) {
                return SocketAddr::new(ip, bound_addr.port());
            }
        }
        bound_addr
    }

    /// Static method to build a connection link from address and public key.
    /// Useful when you don't have a NodeEngine instance (e.g., in CLI tools).
    ///
    /// Format: `plain://nodes.plain?connect=<ip:port>/<pubkey_hex>`
    pub fn build_connection_link(listen_addr: SocketAddr, pubkey: PublicKey) -> String {
        crate::system::build_connection_link(listen_addr, pubkey)
    }

    /// Generate a connection link from a parsed (SocketAddr, PublicKey) tuple.
    /// Useful for roundtrip testing and link reconstruction.
    ///
    /// Format: `plain://nodes.plain?connect=<ip:port>/<pubkey_hex>`
    pub fn generate_connection_link_from_parsed(parsed: (SocketAddr, PublicKey)) -> String {
        Self::build_connection_link(parsed.0, parsed.1)
    }

    /// Register a peer described by a `nodes.plain` connection link.
    ///
    /// The link format is `plain://nodes.plain?connect=<ip:port>/<pubkey_hex>`.
    /// Returns the [`PeerInfo`] on success, or [`NetworkError::InvalidConnectionLink`]
    /// if the link is malformed, the address is invalid, or the peer is oneself.
    pub fn connect_to(
        &self,
        addr: SocketAddr,
        pubkey: PublicKey,
    ) -> Result<PeerInfo, NetworkError> {
        if addr.port() == 0 {
            return Err(NetworkError::InvalidConnectionLink(
                "port 0 is not a valid connection address".to_string(),
            ));
        }
        if addr.ip().is_unspecified() {
            return Err(NetworkError::InvalidConnectionLink(
                "unspecified address is not a valid connection address".to_string(),
            ));
        }
        if pubkey == self.config.keypair.public {
            return Err(NetworkError::InvalidConnectionLink(
                "cannot connect to oneself".to_string(),
            ));
        }
        let peer_info = PeerInfo {
            public_key: pubkey,
            last_seen: Instant::now(),
            address: addr,
            reputation: 0,
        };
        let mut peer_table = self.peer_table.lock().unwrap();
        if let Some(_old) = peer_table.get(&pubkey) {
            peer_table.entry(pubkey).and_modify(|e| {
                e.address = addr;
                e.last_seen = Instant::now();
            });
        } else {
            peer_table.insert(pubkey, peer_info.clone());
        }
        let final_peer = peer_table
            .get(&pubkey)
            .cloned()
            .ok_or(NetworkError::PeerNotFound(pubkey.to_hex()))?;
        drop(peer_table);
        if let Ok(mut routing) = self.routing.lock() {
            routing.update(Contact::new(pubkey, addr), Instant::now());
        }
        Ok(final_peer)
    }

    /// Launch a connection to a peer described by a `nodes.plain` link URI.
    ///
    /// Parses the link, validates it, and registers the peer in the engine's peer table.
    pub fn launch_connection(&self, link: &str) -> Result<PeerInfo, NetworkError> {
        let (addr, pubkey) = crate::system::parse_connection_link(link)
            .ok_or_else(|| NetworkError::InvalidConnectionLink(link.to_string()))?;
        self.connect_to(addr, pubkey)
    }

    /// Parse a connection link URI to extract the address and public key.
    ///
    /// Returns Some((SocketAddr, PublicKey)) if the link is valid.
    pub fn parse_connection_link(link: &str) -> Option<(SocketAddr, PublicKey)> {
        crate::system::parse_connection_link(link)
    }

    /// This node's STUN-discovered public IP:port, if available. `None` when
    /// STUN is disabled, the query failed, or the engine has not started yet.
    /// Connection links prefer this address over the local bound address so
    /// peers behind a NAT remain dialable from the internet.
    pub fn public_addr(&self) -> Option<SocketAddr> {
        *self.public_addr.lock().unwrap()
    }

    /// The address this node advertises to peers: the STUN-discovered public
    /// address when available, otherwise the bound address (with a routable
    /// local IP substituted when the socket is bound to `0.0.0.0`). Relayed
    /// messages carry this so the far side has a peer-table address for
    /// replies, even though the relay is the real next hop.
    pub fn advertised_addr(&self) -> Option<SocketAddr> {
        if let Some(public) = self.public_addr() {
            return Some(public);
        }
        let bound = self.local_addr()?.ok()?;
        Some(self.dialable_address(bound))
    }

    /// Peers currently reached through a relay (target -> relay).
    pub fn relay_routes(&self) -> Arc<Mutex<HashMap<PublicKey, PublicKey>>> {
        Arc::clone(&self.relay_routes)
    }

    /// Register a peer and prove it is actually reachable.
    ///
    /// Unlike [`Self::connect_to`] — which only writes the peer table — this
    /// sends a signed [`NetworkMessage::Ping`] and waits up to `timeout` for a
    /// matching [`NetworkMessage::Pong`] from the same address. Use it when
    /// claiming a peer joined the swarm; a Pong means something is listening.
    pub async fn dial(
        &self,
        addr: SocketAddr,
        pubkey: PublicKey,
        timeout: Duration,
    ) -> Result<PeerInfo, NetworkError> {
        let mut replies = self.subscribe();
        self.connect_to(addr, pubkey)?;

        let nonce = rand::random::<u64>();
        let deadline = tokio::time::Instant::now() + timeout;
        // UDP can drop the Ping or its Pong, so a single probe is not enough
        // to decide a public peer is unreachable. Resend a few times within
        // the caller's budget; the marker stays the same, so a late reply to
        // an earlier probe still satisfies this dial.
        let retry = (timeout / 4).max(Duration::from_millis(100));
        let mut next_ping = tokio::time::Instant::now();
        loop {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                self.metrics.record_peer_dial(false);
                return Err(NetworkError::Timeout);
            }
            if now >= next_ping {
                self.send_to(&pubkey, NetworkMessage::Ping { nonce })?;
                next_ping = now + retry;
            }
            let wake = next_ping.min(deadline);
            let wait = wake.saturating_duration_since(now);
            match tokio::time::timeout(wait, replies.recv()).await {
                // The nonce is random, carried inside an envelope encrypted to
                // this peer, and the envelope identity is the peer we dialed;
                // the source address may legitimately differ from the one we
                // tried when the reply arrives through a relay or a punched
                // NAT mapping, so do not require address equality.
                Ok(Some((_from, NetworkMessage::Pong { nonce: echoed }))) if echoed == nonce => {
                    self.metrics.record_peer_dial(true);
                    return self
                        .get_peer(&pubkey)
                        .ok_or(NetworkError::PeerNotFound(pubkey.to_hex()));
                }
                Ok(Some(_)) => continue,
                Ok(None) => {
                    self.metrics.record_peer_dial(false);
                    return Err(NetworkError::InvalidMessage);
                }
                Err(_) => {
                    if tokio::time::Instant::now() >= deadline {
                        self.metrics.record_peer_dial(false);
                        return Err(NetworkError::Timeout);
                    }
                }
            }
        }
    }

    /// Punch a UDP hole to `target` via a rendezvous peer, then prove the path
    /// with a signed liveness probe.
    ///
    /// STUN only makes cone NATs dialable: two peers behind symmetric NATs
    /// (or CGNAT) cannot receive unsolicited datagrams even when they know each
    /// other's mapped addresses. The rendezvous observes each side's real
    /// source address and tells the other side; both then emit signed
    /// [`crate::net::encode_punch_probe`] datagrams so their NAT mappings open,
    /// after which a normal [`Self::dial`] proves reachability. This is the
    /// UDP simultaneous-open technique used by STUN/TURN-style rendezvous.
    pub async fn hole_punch(
        &self,
        target: PublicKey,
        timeout: Duration,
    ) -> Result<PeerInfo, NetworkError> {
        let rendezvous = {
            let peers = self.peer_table.lock().unwrap();
            peers
                .keys()
                .copied()
                .find(|key| *key != target && !self.relay_routes.lock().unwrap().contains_key(key))
        }
        .ok_or_else(|| NetworkError::PeerNotFound("no rendezvous peer".to_string()))?;
        self.hole_punch_via(rendezvous, target, timeout).await
    }

    /// [`Self::hole_punch`] with an explicitly chosen rendezvous peer.
    pub async fn hole_punch_via(
        &self,
        rendezvous: PublicKey,
        target: PublicKey,
        timeout: Duration,
    ) -> Result<PeerInfo, NetworkError> {
        if target == self.config.keypair.public || rendezvous == self.config.keypair.public {
            return Err(NetworkError::InvalidMessage);
        }
        if self.get_peer(&rendezvous).is_none() {
            return Err(NetworkError::PeerNotFound(rendezvous.to_hex()));
        }

        let (tx, mut rx) = mpsc::channel(1);
        self.punch_waiters.lock().unwrap().insert(target, tx);
        let _ = self.send_to(&rendezvous, NetworkMessage::PunchRequest { target });

        let probe = crate::net::encode_punch_probe(&self.config.keypair, rand::random());
        let known_addr = self.get_peer(&target).map(|peer| peer.address);
        let deadline = tokio::time::Instant::now() + timeout;
        let mut result = Err(NetworkError::Timeout);
        while tokio::time::Instant::now() < deadline {
            if let Some(addr) = known_addr {
                for _ in 0..2 {
                    let _ = self.outgoing_tx.try_send((addr, probe.clone()));
                }
            }
            if let Ok(Some(mapped_addr)) =
                tokio::time::timeout(Duration::from_millis(250), rx.recv()).await
            {
                result = self.dial(mapped_addr, target, Duration::from_secs(2)).await;
            }
            if result.is_ok() {
                break;
            }
        }
        self.punch_waiters.lock().unwrap().remove(&target);
        self.metrics.record_hole_punch(result.is_ok());
        result
    }

    /// Register `target` behind a relay and prove the relayed path with a
    /// signed Ping/Pong round trip.
    ///
    /// This is the fallback when hole punching cannot work (symmetric NAT,
    /// CGNAT, firewall). Both sides receive each other's messages through the
    /// relay, while the inner envelopes stay end-to-end encrypted and signed
    /// so the relay cannot read or forge them.
    pub async fn connect_via_relay(
        &self,
        relay: PublicKey,
        target: PublicKey,
        target_addr: SocketAddr,
        timeout: Duration,
    ) -> Result<PeerInfo, NetworkError> {
        if target == self.config.keypair.public
            || relay == self.config.keypair.public
            || relay == target
        {
            return Err(NetworkError::InvalidMessage);
        }
        if self.get_peer(&relay).is_none() || self.relay_routes.lock().unwrap().contains_key(&relay)
        {
            return Err(NetworkError::PeerNotFound(relay.to_hex()));
        }
        self.connect_to(target_addr, target)?;
        self.relay_routes.lock().unwrap().insert(target, relay);
        let result = self.dial(target_addr, target, timeout).await;
        if result.is_err() {
            self.relay_routes.lock().unwrap().remove(&target);
        }
        result
    }

    /// Try a direct dial, then UDP hole punching, then a relay, in that order.
    /// Returns the first path that answers a signed liveness probe. Use this
    /// for peers (as opposed to public bootstrap nodes) when the network may be
    /// behind NAT.
    pub async fn dial_resilient(
        &self,
        addr: SocketAddr,
        pubkey: PublicKey,
        timeout: Duration,
    ) -> Result<PeerInfo, NetworkError> {
        if let Ok(peer) = self.dial(addr, pubkey, timeout).await {
            return Ok(peer);
        }
        if let Ok(peer) = self.hole_punch(pubkey, Duration::from_millis(1500)).await {
            return Ok(peer);
        }
        let relays: Vec<PublicKey> = {
            let peers = self.peer_table.lock().unwrap();
            peers
                .keys()
                .copied()
                .filter(|key| {
                    *key != pubkey && !self.relay_routes.lock().unwrap().contains_key(key)
                })
                .take(4)
                .collect()
        };
        for relay in relays {
            if let Ok(peer) = self
                .connect_via_relay(relay, pubkey, addr, Duration::from_millis(1500))
                .await
            {
                return Ok(peer);
            }
        }
        self.metrics.record_peer_dial(false);
        Err(NetworkError::Timeout)
    }

    /// Register every configured bootstrap peer without checking reachability.
    ///
    /// Prefer [`Self::connect_bootstrap_verified`]; this method only proves the
    /// entries are syntactically valid, so a dead address is still reported as
    /// a connection.
    pub fn connect_bootstrap(&self) -> Vec<PeerInfo> {
        let mut connected = Vec::new();
        for bootstrap in &self.config.bootstrap_peers {
            if let Ok(peer) = self.connect_to(bootstrap.address, bootstrap.public_key) {
                connected.push(peer);
            } // skip unreachable bootstrap peers
        }
        connected
    }

    /// Dial every configured bootstrap peer in parallel and return only the
    /// peers that answered a liveness probe. This is how a node honestly joins
    /// the swarm: bootstrap nodes are well-known rendezvous points, and an
    /// unreachable one no longer counts as a successful join.
    pub async fn connect_bootstrap_verified(&self) -> Vec<PeerInfo> {
        let mut joins = tokio::task::JoinSet::new();
        for bootstrap in &self.config.bootstrap_peers {
            let engine = self.clone();
            let addr = bootstrap.address;
            let pubkey = bootstrap.public_key;
            joins
                .spawn(async move { engine.dial(addr, pubkey, BOOTSTRAP_DIAL_TIMEOUT).await.ok() });
        }

        let mut connected = Vec::new();
        while let Some(result) = joins.join_next().await {
            if let Ok(Some(peer)) = result {
                connected.push(peer);
            }
        }
        connected
    }

    /// Address of the bound LAN discovery socket, or `None` when discovery is
    /// disabled (loopback-only nodes) or could not bind.
    pub fn discovery_addr(&self) -> Option<SocketAddr> {
        *self.discovery_addr.lock().unwrap()
    }
}

/// Periodically gossip this engine's peer table to every known peer as a
/// [`NetworkMessage::PeerExchange`].
///
/// This is how a rendezvous/bootstrap node shares the swarm: a new node only
/// needs one reachable address, and on the next gossip round it learns every
/// peer the rendezvous node knows (with dialable addresses), without an
/// out-of-band connection link. Entries with unusable addresses (port 0 or an
/// unspecified IP) are dropped by the receiving engine, and an advertised peer
/// still has to authenticate before it is trusted.
pub async fn peer_exchange_loop(engine: NodeEngine, sender_pubkey: PublicKey, interval: Duration) {
    let mut ticker = tokio::time::interval(interval);
    loop {
        ticker.tick().await;
        let peers: Vec<(SocketAddr, PublicKey)> = {
            let table = engine.peer_table();
            let guard = table.lock().unwrap();
            guard.values().map(|p| (p.address, p.public_key)).collect()
        };
        if !peers.is_empty() {
            let _ = engine.broadcast(NetworkMessage::PeerExchange {
                peers,
                sender_pubkey,
            });
        }
    }
}

/// Probe the OS routing table to find a routable local IP address for `addr`.
///
/// Connecting a UDP socket to a public address (without sending anything)
/// makes the kernel select the source IP it would use to reach it — the
/// Bind the UDP socket with large kernel buffers. A single 48 KiB onion
/// message is ~417 cells; with the default Linux receive buffer (~208 KiB) a
/// burst that size overflows and silently loses fragments. Raising both
/// directions to 8 MiB keeps complete circuits (and ordinary traffic bursts)
/// intact. The OS may clamp the request; that is not fatal.
fn bind_udp_socket(addr: SocketAddr) -> std::io::Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(Domain::for_address(addr), Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_nonblocking(true)?;
    let _ = socket.set_recv_buffer_size(8 * 1024 * 1024);
    let _ = socket.set_send_buffer_size(8 * 1024 * 1024);
    if addr.is_ipv6() && addr.ip().is_unspecified() {
        // `[::]` should be dual-stack: accept IPv4 peers as v4-mapped on the
        // same socket (the sender loop maps destinations, the receiver loop
        // unmaps sources). Platforms whose default is v6-only are opted in
        // explicitly rather than silently losing IPv4 reachability.
        let _ = socket.set_only_v6(false);
    }
    socket.bind(&addr.into())?;
    UdpSocket::from_std(socket.into())
}

/// machine's routable LAN IP when behind a NAT. Returns `None` when no such
/// address can be determined.
pub fn best_effort_local_ip(preferred_family: SocketAddr) -> Option<IpAddr> {
    let target = if matches!(preferred_family.ip(), IpAddr::V6(_)) {
        "[2001:4860:4860::8888]:443"
    } else {
        "8.8.8.8:80"
    };
    let socket = std::net::UdpSocket::bind(match preferred_family.ip() {
        IpAddr::V6(_) => "[::]:0",
        IpAddr::V4(_) => "0.0.0.0:0",
    })
    .ok()?;
    socket.connect(target).ok()?;
    socket.local_addr().ok().map(|a| a.ip())
}

impl Clone for NodeEngine {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            peer_table: Arc::clone(&self.peer_table),
            message_sender: self.message_sender.clone(),
            message_receiver: Mutex::new(None), // taken by the sender loop after start()
            outgoing_tx: self.outgoing_tx.clone(),
            outgoing_rx: Mutex::new(None), // taken by the receiver loop after start()
            socket: Mutex::new(self.socket.lock().unwrap().as_ref().map(Arc::clone)),
            public_addr: Mutex::new(*self.public_addr.lock().unwrap()),
            dh_keypair: self.dh_keypair.clone(),
            seq: Arc::clone(&self.seq),
            routing: Arc::clone(&self.routing),
            replay: Arc::clone(&self.replay),
            reassembly: Arc::clone(&self.reassembly),
            onion_assembly: Arc::clone(&self.onion_assembly),
            onion_subscribers: Arc::clone(&self.onion_subscribers),
            onion_request_subscribers: Arc::clone(&self.onion_request_subscribers),
            pending_onion_replies: Arc::clone(&self.pending_onion_replies),
            relay_subscribers: Arc::clone(&self.relay_subscribers),
            running: Arc::clone(&self.running),
            discovery_handle: Mutex::new(None), // join handle is not cloned
            discovery_addr: Mutex::new(*self.discovery_addr.lock().unwrap()),
            subscribers: Arc::clone(&self.subscribers),
            metrics: Arc::clone(&self.metrics),
            relay_routes: Arc::clone(&self.relay_routes),
            punch_waiters: Arc::clone(&self.punch_waiters),
            cover: Mutex::new(self.cover.lock().unwrap().clone()),
            e2ee: Arc::clone(&self.e2ee),
        }
    }
}
