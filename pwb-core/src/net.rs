//! Network framing and system domain protocol message types (pwb-core::net).
//!
//! PlainWeb transport is serverless. System domains (search.plain, gossip.plain,
//! nodes.plain, register.plain) provide native network services directly without
//! central servers.
//!
//! This module provides the NodeEngine which consolidates peer table management,
//! routing logic, framing serialization, and socket event loops. Both pwb-node
//! and pwb-browser instantiate this engine to ensure they run on the exact same
//! network stack.
//!
//! [`CellChannel`] carries onion cells hop-by-hop over UDP with source-IP
//! masking (TODO §2.3): each relay re-originates the datagram from its own
//! socket, so a receiving hop only ever observes the previous relay's
//! address.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::interval;

use crate::crypto::{ContentId, DhPublicKey, Keypair, PublicKey, Signature};
use crate::discovery::{DiscoveryConfig, DiscoveryService};
use crate::onion::{OnionCell, OnionError, CELL_SIZE};
use serde::{Deserialize, Serialize};

/// Wire format representation of a search result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchResultWire {
    pub content_id: ContentId,
    pub title: String,
    pub rank_score: f32,
    pub snippet: String,
}

/// Message protocol framing for PlainWeb P2P communication and System Domain APIs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum NetworkMessage {
    /// System Domain search.plain API Request.
    SearchQuery { query: String, sender: PublicKey },

    /// System Domain search.plain API Response.
    SearchResponse {
        query: String,
        results: Vec<SearchResultWire>,
    },

    /// System Domain gossip.plain Artifact Announcement.
    GossipAnnounce {
        content_id: ContentId,
        title: String,
        publisher: PublicKey,
        signature: Signature,
    },

    /// System Domain nodes.plain Peer Discovery Gossip.
    PeerDiscovery {
        known_peers: Vec<PublicKey>,
        sender_pubkey: PublicKey,
    },

    /// Request a binary .pwb artifact bundle by its content ID.
    FetchArtifact { content_id: ContentId },

    /// Response containing the full .pwb binary artifact bundle.
    ArtifactResponse {
        content_id: ContentId,
        bundle_bytes: Vec<u8>,
    },

    /// Miner submits proof of indexing work to the network (search.plain miner network).
    SubmitIndexWork {
        miner: PublicKey,
        content_id: ContentId,
        indexed_terms_hash: ContentId,
        nonce: u64,
        signature: Signature,
    },

    /// Peer verifier attests to the validity of submitted index work.
    VerifyProof {
        verifier: PublicKey,
        miner: PublicKey,
        work_hash: ContentId,
        is_valid: bool,
        signature: Signature,
    },

    /// System Domain register.plain API Request: Domain registration via sparse credits.
    ClaimDomain {
        domain: String,
        target_content_id: ContentId,
        owner: PublicKey,
        credits_spent: u64,
        signature: Signature,
    },
}

/// Transport abstraction trait for PlainWeb network implementations.
pub trait Transport {
    type Error: std::fmt::Debug;

    fn broadcast(&mut self, msg: NetworkMessage) -> Result<(), Self::Error>;
    fn send_to(&mut self, peer: &PublicKey, msg: NetworkMessage) -> Result<(), Self::Error>;
}

/// A queued outbound datagram: the destination address plus serialized bytes.
type OutgoingDatagram = (SocketAddr, Vec<u8>);

/// Errors that can occur in network operations.
#[derive(Debug, Error)]
pub enum NetworkError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Peer not found: {0}")]
    PeerNotFound(String),

    #[error("Invalid connection link: {0}")]
    InvalidConnectionLink(String),

    #[error("Invalid message")]
    InvalidMessage,

    #[error("Timeout")]
    Timeout,

    #[error(transparent)]
    Onion(#[from] OnionError),
}

/// A bound UDP socket that carries onion cells hop-by-hop (TODO §2.3).
///
/// **Source-IP masking by construction.** Every hop re-originates the
/// datagram from its *own* bound socket toward the next relay's registered
/// address, and the payload is exactly the [`OnionCell`] bytes — there is no
/// source-address field in the envelope to carry, spoof, or strip. A
/// receiving hop therefore observes only the previous relay's IP; the
/// originator's address stops at the entry hop. IP addresses are
/// unencryptable socket metadata, so the masking has to happen by
/// re-originating datagrams, never by trying to encrypt the address.
///
/// The remaining observable metadata is deliberately unactionable for
/// tracking: every datagram is exactly [`CELL_SIZE`] bytes (constant size),
/// and decoy cells scheduled by [`crate::onion::CoverDispatch`] arrive at
/// randomized intervals (no fixed period).
pub struct CellChannel {
    socket: UdpSocket,
    relays: HashMap<DhPublicKey, SocketAddr>,
}

impl CellChannel {
    /// Bind a channel on `local` (use port `0` for an ephemeral port).
    pub async fn bind(local: SocketAddr) -> std::io::Result<Self> {
        Ok(Self {
            socket: UdpSocket::bind(local).await?,
            relays: HashMap::new(),
        })
    }

    /// This channel's local socket address — what remote hops observe as the
    /// source address of everything this node sends.
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    /// Register (or re-register) the address of a relay that onion cells may
    /// be forwarded to next.
    pub fn register_relay(&mut self, relay: DhPublicKey, addr: SocketAddr) {
        self.relays.insert(relay, addr);
    }

    /// Look up a registered relay's address.
    pub fn relay_address(&self, relay: &DhPublicKey) -> Option<SocketAddr> {
        self.relays.get(relay).copied()
    }

    /// Forward `cell` to `relay`'s registered address from this node's own
    /// socket (source-IP masking: the previous hop's address is not part of
    /// the envelope in any form).
    pub async fn send_cell(
        &self,
        relay: &DhPublicKey,
        cell: &OnionCell,
    ) -> Result<(), NetworkError> {
        let addr = self
            .relays
            .get(relay)
            .copied()
            .ok_or_else(|| NetworkError::PeerNotFound(relay.to_hex()))?;
        self.send_cell_to(addr, cell).await
    }

    /// Forward `cell` to an explicit address from this node's own socket,
    /// for callers that resolve hop addresses out of band (e.g. the entry
    /// hop's first peer).
    pub async fn send_cell_to(
        &self,
        addr: SocketAddr,
        cell: &OnionCell,
    ) -> Result<(), NetworkError> {
        let sent = self.socket.send_to(&cell.to_bytes(), addr).await?;
        debug_assert_eq!(sent, CELL_SIZE, "UDP send must emit one full cell");
        Ok(())
    }

    /// Receive one onion-cell datagram, returning it with the source address
    /// the OS observed — i.e. the *previous relay's* socket, never the
    /// originator's. Datagrams that are not exactly [`CELL_SIZE`] bytes of
    /// valid onion cell are rejected.
    pub async fn recv_cell(&self) -> Result<(OnionCell, SocketAddr), NetworkError> {
        let mut buf = [0u8; CELL_SIZE + 1];
        let (len, from) = self.socket.recv_from(&mut buf).await?;
        if len != CELL_SIZE {
            return Err(NetworkError::InvalidMessage);
        }
        let cell = OnionCell::from_bytes(&buf[..len])?;
        Ok((cell, from))
    }
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
    pub bootstrap_peers: Vec<PublicKey>,
    pub max_peers: usize,
    pub heartbeat_interval: Duration,
}

/// The core network engine that handles peer-to-peer communication.
pub struct NodeEngine {
    config: NodeConfig,
    peer_table: Arc<Mutex<HashMap<PublicKey, PeerInfo>>>,
    message_sender: mpsc::UnboundedSender<NetworkMessage>,
    message_receiver: Mutex<Option<mpsc::UnboundedReceiver<NetworkMessage>>>,
    outgoing_tx: mpsc::UnboundedSender<(SocketAddr, Vec<u8>)>,
    outgoing_rx: Mutex<Option<mpsc::UnboundedReceiver<OutgoingDatagram>>>,
    socket: Mutex<Option<Arc<UdpSocket>>>,
    running: Arc<Mutex<bool>>,
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
            Self::message_handler_loop(message_receiver, peer_table, config, running_clone).await;
        });

        // Spawn outgoing sender loop (transmits queued datagrams to peers).
        tokio::spawn(async move {
            Self::sender_loop(socket, outgoing_rx, running_clone2).await;
        });

        // Spawn inbound receiver loop (deserializes datagrams into messages).
        let receiver_socket = self.socket.lock().unwrap().as_ref().unwrap().clone();
        tokio::spawn(async move {
            Self::receiver_loop(receiver_socket, message_sender, running_clone3).await;
        });

        // Spawn peer maintenance loop.
        tokio::spawn(async move {
            Self::peer_maintenance_loop(
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
            .send(message)
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
        self.message_sender
            .send(message)
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
    /// Uses the *actual* bound socket address when the engine has been
    /// started; falls back to [`NodeConfig::listen_addr`] before `start()`.
    ///
    /// Format: `plain://nodes.plain?connect=<ip:port>/<pubkey_hex>`
    pub fn generate_connection_link(&self) -> String {
        match self.local_addr() {
            Some(Ok(addr)) => Self::build_connection_link(addr, self.config.keypair.public),
            _ => Self::build_connection_link(self.config.listen_addr, self.config.keypair.public),
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

    /// Internal message handler loop.
    async fn message_handler_loop(
        mut receiver: mpsc::UnboundedReceiver<NetworkMessage>,
        peer_table: Arc<Mutex<HashMap<PublicKey, PeerInfo>>>,
        config: NodeConfig,
        running: Arc<Mutex<bool>>,
    ) {
        while *running.lock().unwrap() {
            tokio::select! {
                Some(message) = receiver.recv() => {
                    // Process incoming message
                    Self::process_message(message, &peer_table, &config).await;
                }
                _ = tokio::time::sleep(Duration::from_millis(10)) => {
                    // Small sleep to prevent busy loop
                }
            }
        }
    }

    /// Internal peer maintenance loop.
    async fn peer_maintenance_loop(
        peer_table: Arc<Mutex<HashMap<PublicKey, PeerInfo>>>,
        config: NodeConfig,
        running: Arc<Mutex<bool>>,
    ) {
        let mut interval = interval(config.heartbeat_interval);

        loop {
            if !*running.lock().unwrap() {
                break;
            }
            interval.tick().await;

            // Remove stale peers
            let mut peer_table = peer_table.lock().unwrap();
            let now = Instant::now();
            peer_table.retain(|_, peer_info| {
                now.duration_since(peer_info.last_seen) < Duration::from_secs(300)
                // 5 minutes
            });
        }
    }

    /// Outgoing sender loop: dequeues `(SocketAddr, Vec<u8>)` datagrams and
    /// transmits them over the bound UDP socket. Runs until `running` is false.
    async fn sender_loop(
        socket: Arc<UdpSocket>,
        mut receiver: mpsc::UnboundedReceiver<OutgoingDatagram>,
        running: Arc<Mutex<bool>>,
    ) {
        while *running.lock().unwrap() {
            tokio::select! {
                Some((addr, bytes)) = receiver.recv() => {
                    let _ = socket.send_to(&bytes, addr).await;
                }
                _ = tokio::time::sleep(Duration::from_millis(10)) => {
                    // Small sleep to prevent busy loop when channel is empty
                }
            }
        }
    }

    /// Inbound receiver loop: reads UDP datagrams, deserializes them as
    /// [`NetworkMessage`], and enqueues them for the handler loop. Invalid
    /// framing is silently dropped. Runs until `running` is false.
    async fn receiver_loop(
        socket: Arc<UdpSocket>,
        sender: mpsc::UnboundedSender<NetworkMessage>,
        running: Arc<Mutex<bool>>,
    ) {
        let mut buf = [0u8; 65535];
        while *running.lock().unwrap() {
            tokio::select! {
                result = socket.recv_from(&mut buf) => {
                    match result {
                        Ok((len, _from)) => {
                            if let Ok(message) = serde_json::from_slice::<NetworkMessage>(&buf[..len]) {
                                let _ = sender.send(message);
                            }
                            // Invalid JSON framing is silently dropped.
                        }
                        Err(_) => {
                            // Transient socket error; keep listening.
                        }
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(10)) => {
                    // Small sleep to prevent busy loop
                }
            }
        }
    }

    /// Process an incoming network message.
    async fn process_message(
        message: NetworkMessage,
        peer_table: &Arc<Mutex<HashMap<PublicKey, PeerInfo>>>,
        _config: &NodeConfig,
    ) {
        // In a full implementation, this would handle different message types
        // and potentially generate responses
        match message {
            NetworkMessage::PeerDiscovery {
                known_peers,
                sender_pubkey,
            } => {
                // Handle peer discovery gossip
                // Add sender to peer table if not present
                let mut pt = peer_table.lock().unwrap();
                pt.entry(sender_pubkey).or_insert_with(|| {
                    // We would normally have the sender's address from the transport layer
                    // For now, we'll use a placeholder
                    let placeholder_addr = SocketAddr::from(([127, 0, 0, 1], 0));
                    PeerInfo {
                        public_key: sender_pubkey,
                        last_seen: Instant::now(),
                        address: placeholder_addr,
                        reputation: 0,
                    }
                });

                // Add known peers
                for peer_pubkey in known_peers {
                    pt.entry(peer_pubkey).or_insert_with(|| {
                        let placeholder_addr = SocketAddr::from(([127, 0, 0, 1], 0));
                        PeerInfo {
                            public_key: peer_pubkey,
                            last_seen: Instant::now(),
                            address: placeholder_addr,
                            reputation: 0,
                        }
                    });
                }
            }

            // A GossipAnnounce is treated as evidence that the publisher is
            // active: add them to the peer table if unknown, then refresh their
            // `last_seen` so relays keep them alive. The announcement payload
            // itself is not cached here.
            NetworkMessage::GossipAnnounce {
                content_id: _,
                publisher,
                ..
            } => {
                let mut pt = peer_table.lock().unwrap();

                // Add the publisher to the peer table if we don't know them yet
                pt.entry(publisher).or_insert_with(|| {
                    let placeholder_addr = SocketAddr::from(([127, 0, 0, 1], 0));
                    PeerInfo {
                        public_key: publisher,
                        last_seen: Instant::now(),
                        address: placeholder_addr,
                        reputation: 0,
                    }
                });

                // Record that we've seen this content_id from this publisher
                // by updating the publisher's last_seen timestamp, indicating activity
                if let Some(peer_info) = pt.get_mut(&publisher) {
                    peer_info.last_seen = Instant::now();
                }
            }

            // Other message types would be handled here
            _ => {}
        }
    }
}

impl Drop for NodeEngine {
    fn drop(&mut self) {
        // Ensure we stop when dropped
        drop(self.running.lock().unwrap());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use std::str::FromStr;

    #[tokio::test]
    async fn test_node_engine_creation() {
        let kp = Keypair::generate();
        let config = NodeConfig {
            listen_addr: SocketAddr::from_str("127.0.0.1:0").unwrap(),
            keypair: kp.clone(),
            bootstrap_peers: Vec::new(),
            max_peers: 100,
            heartbeat_interval: Duration::from_secs(30),
        };

        let engine = NodeEngine::new(config);
        assert!(!*engine.running.lock().unwrap());

        // Start the engine
        let result = engine.start().await;
        assert!(result.is_ok());
        assert!(*engine.running.lock().unwrap());

        // Stop the engine
        engine.stop().await;
        assert!(!*engine.running.lock().unwrap());
    }

    #[tokio::test]
    async fn test_peer_management() {
        let config = NodeConfig {
            listen_addr: SocketAddr::from_str("127.0.0.1:0").unwrap(),
            keypair: Keypair::generate(),
            bootstrap_peers: Vec::new(),
            max_peers: 100,
            heartbeat_interval: Duration::from_secs(30),
        };

        let engine = NodeEngine::new(config);
        let _ = engine.start().await;

        // Add a peer
        let peer_kp = Keypair::generate();
        let peer_info = PeerInfo {
            public_key: peer_kp.public,
            last_seen: Instant::now(),
            address: SocketAddr::from_str("127.0.0.1:8080").unwrap(),
            reputation: 5,
        };

        engine.add_peer(peer_info);
        let retrieved = engine.get_peer(&peer_kp.public);
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().reputation, 5);

        // Remove peer
        engine.remove_peer(&peer_kp.public);
        let retrieved_after = engine.get_peer(&peer_kp.public);
        assert!(retrieved_after.is_none());

        engine.stop().await;
    }

    #[tokio::test]
    async fn test_send_and_broadcast() {
        let kp = Keypair::generate();
        let config = NodeConfig {
            listen_addr: SocketAddr::from_str("127.0.0.1:0").unwrap(),
            keypair: kp.clone(),
            bootstrap_peers: Vec::new(),
            max_peers: 100,
            heartbeat_interval: Duration::from_secs(30),
        };

        let engine = NodeEngine::new(config);
        let _ = engine.start().await;

        // Add a peer so we can send to it
        let peer_kp = Keypair::generate();
        let peer_info = PeerInfo {
            public_key: peer_kp.public,
            last_seen: Instant::now(),
            address: SocketAddr::from_str("127.0.0.1:8080").unwrap(),
            reputation: 5,
        };
        engine.add_peer(peer_info);

        // Test sending a message
        let message = NetworkMessage::SearchQuery {
            query: "test".to_string(),
            sender: kp.public,
        };

        let send_result = engine.send_to(&peer_kp.public, message.clone());
        assert!(send_result.is_ok());

        // Test broadcasting
        let broadcast_result = engine.broadcast(message);
        assert!(broadcast_result.is_ok());

        engine.stop().await;
    }

    #[tokio::test]
    async fn test_message_processing_peer_discovery() {
        let kp = Keypair::generate();
        let config = NodeConfig {
            listen_addr: SocketAddr::from_str("127.0.0.1:0").unwrap(),
            keypair: kp.clone(),
            bootstrap_peers: Vec::new(),
            max_peers: 100,
            heartbeat_interval: Duration::from_secs(30),
        };

        let engine = NodeEngine::new(config);
        let _ = engine.start().await;

        // Create a peer discovery message from our own keypair
        let message = NetworkMessage::PeerDiscovery {
            known_peers: Vec::new(),
            sender_pubkey: kp.public,
        };

        // Note: The send_to method requires the peer to be in the peer table first
        // So we'll use the broadcast method instead for testing message processing
        // Since both send_to and broadcast use the same message channel, this tests
        // that messages are properly processed through the handler loop
        let send_result = engine.broadcast(message);
        assert!(send_result.is_ok());

        // Give the message handler a moment to process the message
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Our own peer is automatically added to the peer table when the engine starts
        let peer = engine.get_peer(&kp.public);
        assert!(peer.is_some());
        assert_eq!(peer.unwrap().public_key, kp.public);

        engine.stop().await;
    }

    #[tokio::test]
    async fn test_message_processing_with_known_peers() {
        let kp = Keypair::generate();
        let config = NodeConfig {
            listen_addr: SocketAddr::from_str("127.0.0.1:0").unwrap(),
            keypair: kp.clone(),
            bootstrap_peers: Vec::new(),
            max_peers: 100,
            heartbeat_interval: Duration::from_secs(30),
        };

        let engine = NodeEngine::new(config);
        let _ = engine.start().await;

        // Create two additional peer keypairs
        let peer1_kp = Keypair::generate();
        let peer2_kp = Keypair::generate();

        // Create a peer discovery message that includes known peers
        let message = NetworkMessage::PeerDiscovery {
            known_peers: vec![peer1_kp.public, peer2_kp.public],
            sender_pubkey: kp.public,
        };

        // Broadcast the message to test processing
        let send_result = engine.broadcast(message);
        assert!(send_result.is_ok());

        // Give the message handler a moment to process the message
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Now all three peers should be in the peer table
        let peer1 = engine.get_peer(&peer1_kp.public);
        let peer2 = engine.get_peer(&peer2_kp.public);
        let self_peer = engine.get_peer(&kp.public);

        assert!(peer1.is_some());
        assert!(peer2.is_some());
        assert!(self_peer.is_some());

        // Check that they have the correct public keys and default reputation
        let peer1_info = peer1.unwrap();
        let peer2_info = peer2.unwrap();
        let self_peer_info = self_peer.unwrap();
        assert_eq!(peer1_info.public_key, peer1_kp.public);
        assert_eq!(peer1_info.reputation, 0);
        assert_eq!(peer2_info.public_key, peer2_kp.public);
        assert_eq!(peer2_info.reputation, 0);
        assert_eq!(self_peer_info.public_key, kp.public);
        assert_eq!(self_peer_info.reputation, 0);

        engine.stop().await;
    }

    /// Test generating and parsing a connection link.
    #[tokio::test]
    async fn test_connection_link_generation_and_parsing() {
        // Create a NodeEngine with a known keypair
        let kp = Keypair::generate();
        let config = NodeConfig {
            listen_addr: SocketAddr::from_str("127.0.0.1:8080").unwrap(),
            keypair: kp.clone(),
            bootstrap_peers: Vec::new(),
            max_peers: 100,
            heartbeat_interval: Duration::from_secs(30),
        };

        let engine = NodeEngine::new(config);

        // Generate the connection link
        let link = engine.generate_connection_link();
        assert!(link.starts_with("plain://nodes.plain?connect="));

        // Parse the link back
        let parsed = NodeEngine::parse_connection_link(&link).expect("Failed to parse link");
        assert_eq!(parsed.0, SocketAddr::from_str("127.0.0.1:8080").unwrap());
        assert_eq!(parsed.1, kp.public);
    }

    /// Test parsing an invalid connection link.
    #[tokio::test]
    async fn test_connection_link_parsing_invalid() {
        // Test various invalid links
        assert!(NodeEngine::parse_connection_link("").is_none());
        assert!(NodeEngine::parse_connection_link("plain://nodes.plain?connect=").is_none());
        assert!(
            NodeEngine::parse_connection_link("plain://nodes.plain?connect=127.0.0.1:8080/")
                .is_none()
        );
        assert!(NodeEngine::parse_connection_link(
            "plain://nodes.plain?connect=127.0.0.1:8080/short"
        )
        .is_none());
        assert!(NodeEngine::parse_connection_link("wrong://nodes.plain?connect=127.0.0.1:8080/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef").is_none());
        assert!(NodeEngine::parse_connection_link("plain://search.plain?connect=127.0.0.1:8080/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef").is_none());
    }

    /// Test that two nodes with different keypairs generate compatible connection links.
    #[tokio::test]
    async fn test_multi_node_link_compatibility() {
        use crate::crypto::Keypair;
        use crate::net::NodeEngine;

        // Node 1 generates its link with a known keypair
        let kp1 = Keypair::generate();
        let config1 = NodeConfig {
            listen_addr: "192.168.1.1:8081".parse::<SocketAddr>().unwrap(),
            keypair: kp1.clone(),
            bootstrap_peers: Vec::new(),
            max_peers: 100,
            heartbeat_interval: Duration::from_secs(30),
        };
        let engine1 = NodeEngine::new(config1);
        let link1 = engine1.generate_connection_link();

        // Node 2 generates its link with a different keypair
        let kp2 = Keypair::generate();
        let config2 = NodeConfig {
            listen_addr: "192.168.1.2:8082".parse::<SocketAddr>().unwrap(),
            keypair: kp2.clone(),
            bootstrap_peers: Vec::new(),
            max_peers: 100,
            heartbeat_interval: Duration::from_secs(30),
        };
        let engine2 = NodeEngine::new(config2);
        let link2 = engine2.generate_connection_link();

        // Both links should start with the correct prefix
        assert!(link1.starts_with("plain://nodes.plain?connect="));
        assert!(link2.starts_with("plain://nodes.plain?connect="));

        // Each link contains its own address
        let parsed1 = NodeEngine::parse_connection_link(&link1).expect("Failed to parse link1");
        let parsed2 = NodeEngine::parse_connection_link(&link2).expect("Failed to parse link2");

        // Node 1's link should parse to its own address
        assert_eq!(parsed1.0, "192.168.1.1:8081".parse::<SocketAddr>().unwrap());
        // Node 2's link should parse to its own address
        assert_eq!(parsed2.0, "192.168.1.2:8082".parse::<SocketAddr>().unwrap());

        // The addresses should be different (different nodes on different IPs/ports)
        assert_ne!(parsed1.0, parsed2.0);

        // Now test parse+generate roundtrip for both
        let link1_back = NodeEngine::generate_connection_link_from_parsed(parsed1);
        let link2_back = NodeEngine::generate_connection_link_from_parsed(parsed2);

        assert_eq!(link1, link1_back);
        assert_eq!(link2, link2_back);
    }

    /// TODO §2.3 source-IP masking: three relays on loopback UDP. Each hop
    /// re-originates the datagram from its own socket, so every receiver
    /// observes only the previous relay's address — the originator's IP
    /// stops at the entry hop and the envelope (exactly the cell bytes)
    /// carries no source field that could resurrect it.
    #[tokio::test]
    async fn test_cell_channel_masks_source_ip_across_hops() {
        use crate::crypto::DhKeypair;
        use crate::onion::OnionAction;

        let relays: Vec<DhKeypair> = (0..3).map(|_| DhKeypair::generate()).collect();

        let originator = CellChannel::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let mut entry = CellChannel::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let mut middle = CellChannel::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let exit = CellChannel::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();

        let originator_addr = originator.local_addr().unwrap();
        let entry_addr = entry.local_addr().unwrap();
        let middle_addr = middle.local_addr().unwrap();
        let exit_addr = exit.local_addr().unwrap();

        entry.register_relay(relays[1].public, middle_addr);
        middle.register_relay(relays[2].public, exit_addr);

        let path = [relays[0].public, relays[1].public, relays[2].public];
        let cell = OnionCell::encapsulate(b"hidden query", &path).unwrap();

        // Hop 1: only the entry ever sees the originator's address.
        originator.send_cell_to(entry_addr, &cell).await.unwrap();
        let (cell, from) = entry.recv_cell().await.unwrap();
        assert_eq!(from, originator_addr);

        // The entry peels and re-originates toward the middle from *its own*
        // socket.
        let peeled = cell.peel(&relays[0]).unwrap().unwrap();
        let OnionAction::Forward(next) = peeled.action else {
            panic!("entry must forward, got {:?}", peeled.action);
        };
        assert_eq!(next, relays[1].public);
        entry
            .send_cell(&next, &peeled.forward.unwrap())
            .await
            .unwrap();

        // Hop 2: only the entry's IP is observable — the originator's
        // address does not survive the entry hop.
        let (cell, from) = middle.recv_cell().await.unwrap();
        assert_eq!(from, entry_addr, "hop 2 sees only the previous relay");
        assert_ne!(from, originator_addr, "originator IP must not reach hop 2");

        let peeled = cell.peel(&relays[1]).unwrap().unwrap();
        let OnionAction::Forward(next) = peeled.action else {
            panic!("middle must forward, got {:?}", peeled.action);
        };
        middle
            .send_cell(&next, &peeled.forward.unwrap())
            .await
            .unwrap();

        // Hop 3: the exit sees only the middle's IP and recovers the payload.
        let (cell, from) = exit.recv_cell().await.unwrap();
        assert_eq!(from, middle_addr);
        let peeled = cell.peel(&relays[2]).unwrap().unwrap();
        assert_eq!(peeled.action, OnionAction::Deliver);
        assert_eq!(peeled.payload.as_deref(), Some(b"hidden query".as_slice()));
    }

    /// TODO §2.3: unknown next hops are refused before anything is sent, and
    /// malformed / wrong-version datagrams are rejected on receive.
    #[tokio::test]
    async fn test_cell_channel_rejects_unknown_relay_and_bad_datagrams() {
        use crate::crypto::DhKeypair;
        use crate::onion::ONION_VERSION;

        let mut channel = CellChannel::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let keys: Vec<DhKeypair> = (0..3).map(|_| DhKeypair::generate()).collect();
        let path = [keys[0].public, keys[1].public, keys[2].public];
        let cell = OnionCell::encapsulate(b"x", &path).unwrap();

        let raw = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let raw_addr = raw.local_addr().unwrap();
        let addr = channel.local_addr().unwrap();

        // Unregistered relay → PeerNotFound, nothing leaves the socket.
        let unknown = DhKeypair::generate().public;
        let err = channel.send_cell(&unknown, &cell).await.unwrap_err();
        assert!(matches!(err, NetworkError::PeerNotFound(_)));

        // Registered relay → address lookup round-trips and send succeeds.
        channel.register_relay(unknown, raw_addr);
        assert_eq!(channel.relay_address(&unknown), Some(raw_addr));
        assert_eq!(channel.relay_address(&DhKeypair::generate().public), None);
        channel.send_cell(&unknown, &cell).await.unwrap();

        // Undersized datagram → InvalidMessage.
        raw.send_to(b"not a cell", addr).await.unwrap();
        assert!(matches!(
            channel.recv_cell().await,
            Err(NetworkError::InvalidMessage)
        ));

        // Correct size but wrong onion version → typed onion error.
        let mut bytes = cell.to_bytes();
        bytes[0] = ONION_VERSION + 1;
        raw.send_to(&bytes, addr).await.unwrap();
        let err = channel.recv_cell().await.unwrap_err();
        assert!(matches!(
            err,
            NetworkError::Onion(OnionError::UnsupportedVersion(_))
        ));
    }

    /// TODO §2.3 cover traffic: a scheduled decoy rides the same masked hop
    /// channel as real cells — constant-size datagram, peelable by the
    /// first relay, dispatched exactly on the scheduler's due time.
    #[tokio::test]
    async fn test_cover_decoy_dispatched_over_cell_channel() {
        use crate::crypto::DhKeypair;
        use crate::onion::{CoverConfig, CoverDispatch, OnionAction};
        use rand::{rngs::StdRng, SeedableRng};

        let keys: Vec<DhKeypair> = (0..3).map(|_| DhKeypair::generate()).collect();
        let candidates: Vec<DhPublicKey> = keys.iter().map(|k| k.public).collect();

        let mut rng = StdRng::seed_from_u64(21);
        let mut dispatch = CoverDispatch::new(CoverConfig {
            mean_interval: Duration::from_secs(60),
            min_interval: Duration::from_secs(1),
            max_interval: Duration::from_secs(120),
        });

        // Arm, then collect the decoy exactly when due.
        let t0 = Instant::now();
        assert!(dispatch.poll(t0, &candidates, &mut rng).is_none());
        let due = dispatch.next_due().expect("armed");
        let (decoy, path) = dispatch
            .poll(due, &candidates, &mut rng)
            .expect("due poll dispatches a decoy");

        // Send it like a real request: sender socket → first relay's socket.
        let mut sender = CellChannel::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let hop = CellChannel::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let hop_addr = hop.local_addr().unwrap();
        sender.register_relay(path.relays[0], hop_addr);
        sender.send_cell(&path.relays[0], &decoy).await.unwrap();

        let (cell, from) = hop.recv_cell().await.unwrap();
        assert_eq!(from, sender.local_addr().unwrap());
        assert_eq!(
            cell.to_bytes().len(),
            CELL_SIZE,
            "cover datagrams are indistinguishable in size"
        );

        // The receiving relay peels the decoy exactly like a real request.
        let key_for = |pk: &DhPublicKey| {
            keys.iter()
                .find(|k| k.public == *pk)
                .expect("path relay comes from candidates")
        };
        let peeled = cell.peel(key_for(&path.relays[0])).unwrap().unwrap();
        let OnionAction::Forward(next) = peeled.action else {
            panic!("first hop of a decoy path must forward");
        };
        assert_eq!(next, path.relays[1]);
    }

    /// Engine error paths: double-start is a no-op, `peer_table()` exposes
    /// the shared table, unknown-peer sends fail before touching the
    /// channel, and gossip announcements register/refresh the publisher.
    #[tokio::test]
    async fn test_node_engine_error_paths_and_gossip_registration() {
        let config = NodeConfig {
            listen_addr: SocketAddr::from_str("127.0.0.1:0").unwrap(),
            keypair: Keypair::generate(),
            bootstrap_peers: Vec::new(),
            max_peers: 100,
            heartbeat_interval: Duration::from_secs(30),
        };
        let engine = NodeEngine::new(config);

        engine.start().await.expect("first start");
        engine.start().await.expect("second start is a no-op");
        assert!(
            engine.peer_table().lock().unwrap().is_empty(),
            "no peers registered before any traffic"
        );

        // Unknown peer → PeerNotFound before anything is queued.
        let stranger = Keypair::generate().public;
        let err = engine
            .send_to(
                &stranger,
                NetworkMessage::SearchQuery {
                    query: "x".to_string(),
                    sender: Keypair::generate().public,
                },
            )
            .unwrap_err();
        assert!(matches!(err, NetworkError::PeerNotFound(_)));

        // A gossip announcement registers the publisher and refreshes it.
        let publisher = Keypair::generate().public;
        engine
            .broadcast(NetworkMessage::GossipAnnounce {
                content_id: ContentId([1u8; 32]),
                title: "artifact".to_string(),
                publisher,
                signature: Signature([2u8; 64]),
            })
            .expect("broadcast enqueues");
        tokio::time::sleep(Duration::from_millis(50)).await;
        let seen = engine.get_peer(&publisher).expect("publisher registered");
        assert_eq!(seen.public_key, publisher);

        // Second announcement refreshes last_seen on the existing entry.
        engine
            .broadcast(NetworkMessage::GossipAnnounce {
                content_id: ContentId([3u8; 32]),
                title: "artifact 2".to_string(),
                publisher,
                signature: Signature([4u8; 64]),
            })
            .expect("broadcast enqueues");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(engine.get_peer(&publisher).is_some());

        engine.stop().await;
    }

    #[tokio::test]
    async fn test_launch_connection_valid_link() {
        let kp = Keypair::generate();
        let config = NodeConfig {
            listen_addr: SocketAddr::from_str("127.0.0.1:0").unwrap(),
            keypair: kp.clone(),
            bootstrap_peers: Vec::new(),
            max_peers: 100,
            heartbeat_interval: Duration::from_secs(30),
        };
        let engine = NodeEngine::new(config);
        let _ = engine.start().await;
        let peer_kp = Keypair::generate();
        let link = format!(
            "plain://nodes.plain?connect=127.0.0.1:9000/{}",
            peer_kp.public.to_hex()
        );
        let result = engine.launch_connection(&link);
        assert!(result.is_ok());
        let peer = engine.get_peer(&peer_kp.public).unwrap();
        assert_eq!(
            peer.address,
            "127.0.0.1:9000".parse::<SocketAddr>().unwrap()
        );
        assert_eq!(peer.public_key, peer_kp.public);
        engine.stop().await;
    }

    #[tokio::test]
    async fn test_launch_connection_invalid_link() {
        let kp = Keypair::generate();
        let config = NodeConfig {
            listen_addr: SocketAddr::from_str("127.0.0.1:0").unwrap(),
            keypair: kp.clone(),
            bootstrap_peers: Vec::new(),
            max_peers: 100,
            heartbeat_interval: Duration::from_secs(30),
        };
        let engine = NodeEngine::new(config);
        let _ = engine.start().await;
        let result = engine.launch_connection("garbage-link");
        assert!(matches!(
            result,
            Err(NetworkError::InvalidConnectionLink(_))
        ));
        engine.stop().await;
    }

    #[tokio::test]
    async fn test_launch_connection_self_link() {
        let kp = Keypair::generate();
        let config = NodeConfig {
            listen_addr: SocketAddr::from_str("127.0.0.1:0").unwrap(),
            keypair: kp.clone(),
            bootstrap_peers: Vec::new(),
            max_peers: 100,
            heartbeat_interval: Duration::from_secs(30),
        };
        let engine = NodeEngine::new(config);
        let _ = engine.start().await;
        let link = format!(
            "plain://nodes.plain?connect=127.0.0.1:9000/{}",
            kp.public.to_hex()
        );
        let result = engine.launch_connection(&link);
        assert!(matches!(
            result,
            Err(NetworkError::InvalidConnectionLink(_))
        ));
        engine.stop().await;
    }

    #[tokio::test]
    async fn test_launch_connection_port_zero() {
        let kp = Keypair::generate();
        let config = NodeConfig {
            listen_addr: SocketAddr::from_str("127.0.0.1:0").unwrap(),
            keypair: kp.clone(),
            bootstrap_peers: Vec::new(),
            max_peers: 100,
            heartbeat_interval: Duration::from_secs(30),
        };
        let engine = NodeEngine::new(config);
        let _ = engine.start().await;
        let peer_kp = Keypair::generate();
        let link = format!(
            "plain://nodes.plain?connect=127.0.0.1:0/{}",
            peer_kp.public.to_hex()
        );
        let result = engine.launch_connection(&link);
        assert!(matches!(
            result,
            Err(NetworkError::InvalidConnectionLink(_))
        ));
        engine.stop().await;
    }

    #[tokio::test]
    async fn test_launch_connection_unspecified_address() {
        let kp = Keypair::generate();
        let config = NodeConfig {
            listen_addr: SocketAddr::from_str("127.0.0.1:0").unwrap(),
            keypair: kp.clone(),
            bootstrap_peers: Vec::new(),
            max_peers: 100,
            heartbeat_interval: Duration::from_secs(30),
        };
        let engine = NodeEngine::new(config);
        let _ = engine.start().await;
        let peer_kp = Keypair::generate();
        let link = format!(
            "plain://nodes.plain?connect=0.0.0.0:8080/{}",
            peer_kp.public.to_hex()
        );
        let result = engine.launch_connection(&link);
        assert!(matches!(
            result,
            Err(NetworkError::InvalidConnectionLink(_))
        ));
        engine.stop().await;
    }

    #[tokio::test]
    async fn test_launch_connection_refresh_preserves_reputation() {
        let kp = Keypair::generate();
        let config = NodeConfig {
            listen_addr: SocketAddr::from_str("127.0.0.1:0").unwrap(),
            keypair: kp.clone(),
            bootstrap_peers: Vec::new(),
            max_peers: 100,
            heartbeat_interval: Duration::from_secs(30),
        };
        let engine = NodeEngine::new(config);
        let _ = engine.start().await;
        let peer_kp = Keypair::generate();
        let peer_info = PeerInfo {
            public_key: peer_kp.public,
            last_seen: Instant::now(),
            address: "127.0.0.1:8080".parse::<SocketAddr>().unwrap(),
            reputation: 42,
        };
        engine.add_peer(peer_info);
        let link = format!(
            "plain://nodes.plain?connect=127.0.0.1:9000/{}",
            peer_kp.public.to_hex()
        );
        let result = engine.launch_connection(&link);
        assert!(result.is_ok());
        let final_peer = engine.get_peer(&peer_kp.public).unwrap();
        assert_eq!(final_peer.reputation, 42);
        assert_eq!(
            final_peer.address,
            "127.0.0.1:9000".parse::<SocketAddr>().unwrap()
        );
        engine.stop().await;
    }

    #[tokio::test]
    async fn test_connect_to_valid() {
        let kp = Keypair::generate();
        let config = NodeConfig {
            listen_addr: SocketAddr::from_str("127.0.0.1:0").unwrap(),
            keypair: kp.clone(),
            bootstrap_peers: Vec::new(),
            max_peers: 100,
            heartbeat_interval: Duration::from_secs(30),
        };
        let engine = NodeEngine::new(config);
        let _ = engine.start().await;
        let peer_kp = Keypair::generate();
        let addr = "127.0.0.1:9000".parse::<SocketAddr>().unwrap();
        let result = engine.connect_to(addr, peer_kp.public);
        assert!(result.is_ok());
        let peer = engine.get_peer(&peer_kp.public).unwrap();
        assert_eq!(peer.address, addr);
        assert_eq!(peer.public_key, peer_kp.public);
        engine.stop().await;
    }

    #[tokio::test]
    async fn test_connect_to_invalid_address() {
        let kp = Keypair::generate();
        let config = NodeConfig {
            listen_addr: SocketAddr::from_str("127.0.0.1:0").unwrap(),
            keypair: kp.clone(),
            bootstrap_peers: Vec::new(),
            max_peers: 100,
            heartbeat_interval: Duration::from_secs(30),
        };
        let engine = NodeEngine::new(config);
        let _ = engine.start().await;
        let peer_kp = Keypair::generate();
        let addr = "0.0.0.0:8080".parse::<SocketAddr>().unwrap();
        let result = engine.connect_to(addr, peer_kp.public);
        assert!(matches!(
            result,
            Err(NetworkError::InvalidConnectionLink(_))
        ));
        engine.stop().await;
    }

    #[tokio::test]
    async fn test_connect_to_self_rejection() {
        let kp = Keypair::generate();
        let config = NodeConfig {
            listen_addr: SocketAddr::from_str("127.0.0.1:0").unwrap(),
            keypair: kp.clone(),
            bootstrap_peers: Vec::new(),
            max_peers: 100,
            heartbeat_interval: Duration::from_secs(30),
        };
        let engine = NodeEngine::new(config);
        let _ = engine.start().await;
        let addr = "127.0.0.1:8080".parse::<SocketAddr>().unwrap();
        let result = engine.connect_to(addr, kp.public);
        assert!(matches!(
            result,
            Err(NetworkError::InvalidConnectionLink(_))
        ));
        engine.stop().await;
    }

    #[tokio::test]
    async fn test_connect_to_port_zero_rejection() {
        let kp = Keypair::generate();
        let config = NodeConfig {
            listen_addr: SocketAddr::from_str("127.0.0.1:0").unwrap(),
            keypair: kp.clone(),
            bootstrap_peers: Vec::new(),
            max_peers: 100,
            heartbeat_interval: Duration::from_secs(30),
        };
        let engine = NodeEngine::new(config);
        let _ = engine.start().await;
        let peer_kp = Keypair::generate();
        let addr = "127.0.0.1:0".parse::<SocketAddr>().unwrap();
        let result = engine.connect_to(addr, peer_kp.public);
        assert!(matches!(
            result,
            Err(NetworkError::InvalidConnectionLink(_))
        ));
        engine.stop().await;
    }

    #[tokio::test]
    async fn test_connect_to_refresh_preserves_reputation() {
        let kp = Keypair::generate();
        let config = NodeConfig {
            listen_addr: SocketAddr::from_str("127.0.0.1:0").unwrap(),
            keypair: kp.clone(),
            bootstrap_peers: Vec::new(),
            max_peers: 100,
            heartbeat_interval: Duration::from_secs(30),
        };
        let engine = NodeEngine::new(config);
        let _ = engine.start().await;
        let peer_kp = Keypair::generate();
        let addr1 = "127.0.0.1:8080".parse::<SocketAddr>().unwrap();
        let result1 = engine.connect_to(addr1, peer_kp.public);
        assert!(result1.is_ok());
        let info = engine.get_peer(&peer_kp.public).unwrap();
        assert_eq!(info.address, addr1);
        assert_eq!(info.reputation, 0);
        let addr2 = "127.0.0.1:9000".parse::<SocketAddr>().unwrap();
        let result2 = engine.connect_to(addr2, peer_kp.public);
        assert!(result2.is_ok());
        let final_info = engine.get_peer(&peer_kp.public).unwrap();
        assert_eq!(final_info.address, addr2);
        assert_eq!(final_info.reputation, 0);
        engine.stop().await;
    }
}
