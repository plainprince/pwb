//! The NodeEngine: peer table, lifecycle, messaging, and connection links.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use crate::crypto::{Keypair, PublicKey};
use crate::discovery::{DiscoveryConfig, DiscoveryService};
use crate::stun::discover_mapped_address;

use super::loops;
use super::messages::{NetworkError, NetworkMessage, OutgoingDatagram};

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
    message_sender: mpsc::UnboundedSender<(SocketAddr, NetworkMessage)>,
    message_receiver: Mutex<Option<mpsc::UnboundedReceiver<(SocketAddr, NetworkMessage)>>>,
    outgoing_tx: mpsc::UnboundedSender<(SocketAddr, Vec<u8>)>,
    outgoing_rx: Mutex<Option<mpsc::UnboundedReceiver<OutgoingDatagram>>>,
    socket: Mutex<Option<Arc<UdpSocket>>>,
    /// This node's public IP:port as learned via STUN (NAT traversal). `None`
    /// when STUN is disabled or the query failed; connection links fall back
    /// to the local bound address in that case.
    pub(crate) public_addr: Mutex<Option<SocketAddr>>,
    pub(super) running: Arc<Mutex<bool>>,
    discovery_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl NodeEngine {
    /// Create a new NodeEngine with the given configuration.
    pub fn new(config: NodeConfig) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let (outgoing_tx, outgoing_rx) = mpsc::unbounded_channel();

        Self {
            config,
            peer_table: Arc::new(Mutex::new(HashMap::new())),
            message_sender: tx,
            message_receiver: Mutex::new(Some(rx)),
            outgoing_tx,
            outgoing_rx: Mutex::new(Some(outgoing_rx)),
            socket: Mutex::new(None),
            public_addr: Mutex::new(None),
            running: Arc::new(Mutex::new(false)),
            discovery_handle: Mutex::new(None),
        }
    }

    /// Start the network engine (spawns background tasks).
    ///
    /// Binds the UDP listen socket, spawns the message receiver loop (deserializes
    /// inbound `NetworkMessage` datagrams and hands them to the handler), the
    /// outgoing sender loop (transmits queued datagrams to peers), the peer
    /// maintenance loop, and a [`DiscoveryService`] that shares this engine's
    /// peer table so LAN beacons immediately populate the peer table. Bootstrap
    /// peers are contacted via [`connect_to`](Self::connect_to) once their
    /// addresses are learned (discovery or an explicit connection link).
    pub async fn start(&self) -> Result<(), NetworkError> {
        if self.is_running() {
            return Ok(());
        }
        self.set_running();

        // Bind the UDP listen socket for all network I/O.
        let socket = Arc::new(UdpSocket::bind(self.config.listen_addr).await?);
        *self.socket.lock().unwrap() = Some(Arc::clone(&socket));

        // NAT traversal: ask the STUN server for our public IP:port so
        // connection links advertise a dialable public address instead of a
        // private LAN address. Best-effort: a failed STUN query falls back to
        // the local bound address.
        if let Some(stun_server) = self.config.stun_server {
            if let Some(mapped) = discover_mapped_address(&socket, stun_server, Duration::from_secs(5)).await {
                *self.public_addr.lock().unwrap() = Some(mapped);
            }
        }

        // Take the receiver so we can move it into the async task
        let message_receiver = self
            .message_receiver
            .lock()
            .unwrap()
            .take()
            .expect("NodeEngine already started");

        // Take the outgoing receiver for the sender loop.
        let outgoing_rx = self
            .outgoing_rx
            .lock()
            .unwrap()
            .take()
            .expect("NodeEngine already started");

        let peer_table = Arc::clone(&self.peer_table);
        let peer_table_for_maintenance = Arc::clone(&self.peer_table);
        let peer_table_for_discovery = Arc::clone(&self.peer_table);
        let config = self.config.clone();
        let config_for_maintenance = self.config.clone();
        let running_clone = Arc::clone(&self.running);
        let running_clone2 = Arc::clone(&self.running);
        let running_clone3 = Arc::clone(&self.running);
        let running_clone4 = Arc::clone(&self.running);
        let message_sender = self.message_sender.clone();

        // Spawn message handler loop (processes inbound messages).
        tokio::spawn(async move {
            loops::message_handler_loop(message_receiver, peer_table, config, running_clone).await;
        });
        // Spawn outgoing sender loop (transmits queued datagrams to peers).
        tokio::spawn(async move {
            loops::sender_loop(socket, outgoing_rx, running_clone2).await;
        });

        // Spawn inbound receiver loop (deserializes datagrams into messages).
        let receiver_socket = self.socket.lock().unwrap().as_ref().unwrap().clone();
        tokio::spawn(async move {
            loops::receiver_loop(receiver_socket, message_sender, running_clone3).await;
        });

        // Spawn peer maintenance loop.
        tokio::spawn(async move {
            loops::peer_maintenance_loop(
                peer_table_for_maintenance,
                config_for_maintenance,
                running_clone4,
            )
            .await;
        });

        // Start discovery sharing this engine's peer table.
        let discovery = DiscoveryService::bind_with_table(
            DiscoveryConfig {
                listen_port: self.config.listen_addr.port(),
                bind_port: 0, // ephemeral discovery port so multiple nodes can co-exist
                beacon_interval: self.config.heartbeat_interval,
                max_age: crate::discovery::BEACON_MAX_AGE,
            },
            self.config.keypair.clone(),
            peer_table_for_discovery,
        )
        .await
        .map_err(|e| NetworkError::Io(std::io::Error::other(e)))?;
        let handle = discovery.spawn();
        *self.discovery_handle.lock().unwrap() = Some(handle);

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

    /// This engine's bound UDP socket address (the address peers observe as
    /// the source of this node's datagrams). Returns `None` before `start()`
    /// has bound the socket.
    pub fn local_addr(&self) -> Option<std::io::Result<SocketAddr>> {
        self.socket.lock().unwrap().as_ref().map(|s| s.local_addr())
    }

    /// Send a message to a specific peer.
    ///
    /// Serializes the [`NetworkMessage`] as JSON and transmits it to the
    /// peer's registered UDP address. Also enqueues the message locally so
    /// the handler loop can process it (preserving existing in-process
    /// behavior). Returns [`NetworkError::PeerNotFound`] when the peer is
    /// not in the peer table.
    pub fn send_to(&self, peer: &PublicKey, message: NetworkMessage) -> Result<(), NetworkError> {
        // Check if peer exists
        let peer_table = self.peer_table.lock().unwrap();
        let Some(addr) = peer_table.get(peer).map(|p| p.address) else {
            return Err(NetworkError::PeerNotFound(peer.to_hex()));
        };
        drop(peer_table);

        let bytes = serde_json::to_vec(&message).map_err(NetworkError::Serialization)?;

        // Transmit over the wire if the engine is started.
        let _ = self.outgoing_tx.send((addr, bytes));

        // Also enqueue locally for the handler loop (in-process delivery).
        self.message_sender
            .send((addr, message))
            .map_err(|_| NetworkError::InvalidMessage)?;
        Ok(())
    }

    /// Broadcast a message to all connected peers.
    ///
    /// Serializes the [`NetworkMessage`] as JSON and transmits it to every
    /// peer's registered UDP address. Also enqueues the message locally.
    pub fn broadcast(&self, message: NetworkMessage) -> Result<(), NetworkError> {
        let bytes = serde_json::to_vec(&message).map_err(NetworkError::Serialization)?;
        let peers = {
            let peer_table = self.peer_table.lock().unwrap();
            peer_table.values().map(|p| p.address).collect::<Vec<_>>()
        };
        for addr in peers {
            let _ = self.outgoing_tx.send((addr, bytes.clone()));
        }

        // Also enqueue locally for the handler loop (in-process delivery).
        let local_from = self
            .local_addr()
            .and_then(|r| r.ok())
            .unwrap_or(self.config.listen_addr);
        self.message_sender
            .send((local_from, message))
            .map_err(|_| NetworkError::InvalidMessage)?;
        Ok(())
    }

    /// Get a reference to the peer table (read-only).
    pub fn peer_table(&self) -> Arc<Mutex<HashMap<PublicKey, PeerInfo>>> {
        Arc::clone(&self.peer_table)
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
        let guard = self.public_addr.lock().unwrap();
        let addr = (*guard).or_else(|| self.local_addr().and_then(|r| r.ok()));
        drop(guard);
        match addr {
            Some(addr) => Self::build_connection_link(addr, self.config.keypair.public),
            None => Self::build_connection_link(self.config.listen_addr, self.config.keypair.public),
        }
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

    /// Dial every configured bootstrap peer and register them in the peer
    /// table. This is how a new node joins the swarm without an out-of-band
    /// introduction: bootstrap nodes are well-known rendezvous points.
    ///
    /// Returns the list of peers successfully registered. Failures for
    /// individual bootstrap peers are skipped (one unreachable bootstrap
    /// does not block joining via another).
    pub fn connect_bootstrap(&self) -> Vec<PeerInfo> {
        let mut connected = Vec::new();
        for bootstrap in &self.config.bootstrap_peers {
            if let Ok(peer) = self.connect_to(bootstrap.address, bootstrap.public_key) {
                connected.push(peer);
            } // skip unreachable bootstrap peers
        }
        connected
    }
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
            socket: Mutex::new(
                self.socket
                    .lock()
                    .unwrap()
                    .as_ref()
                    .map(Arc::clone)
            ),
            public_addr: Mutex::new(*self.public_addr.lock().unwrap()),
            running: Arc::clone(&self.running),
            discovery_handle: Mutex::new(None), // join handle is not cloned
        }
    }
}

impl Drop for NodeEngine {
    fn drop(&mut self) {
        // Ensure we stop when dropped
        drop(self.running.lock().unwrap());
    }
}
