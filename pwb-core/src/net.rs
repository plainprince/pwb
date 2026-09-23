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

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::sync::mpsc;
use tokio::time::interval;

use crate::crypto::{ContentId, Keypair, PublicKey, Signature};

/// Wire format representation of a search result.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchResultWire {
    pub content_id: ContentId,
    pub title: String,
    pub rank_score: f32,
    pub snippet: String,
}

/// Message protocol framing for PlainWeb P2P communication and System Domain APIs.
#[derive(Debug, Clone, PartialEq)]
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

/// Errors that can occur in network operations.
#[derive(Debug, Error)]
pub enum NetworkError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Peer not found: {0}")]
    PeerNotFound(String),

    #[error("Invalid message")]
    InvalidMessage,

    #[error("Timeout")]
    Timeout,
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
    running: Arc<Mutex<bool>>,
}

impl NodeEngine {
    /// Create a new NodeEngine with the given configuration.
    pub fn new(config: NodeConfig) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();

        Self {
            config,
            peer_table: Arc::new(Mutex::new(HashMap::new())),
            message_sender: tx,
            message_receiver: Mutex::new(Some(rx)),
            running: Arc::new(Mutex::new(false)),
        }
    }

    /// Start the network engine (spawns background tasks).
    pub async fn start(&self) -> Result<(), NetworkError> {
        let mut running = self.running.lock().unwrap();
        if *running {
            return Ok(());
        }
        *running = true;

        // Clone Arc references for async tasks - clone everything we need first
        let peer_table = Arc::clone(&self.peer_table);
        let peer_table_for_maintenance = Arc::clone(&self.peer_table);
        let config = self.config.clone();
        let config_for_maintenance = self.config.clone();
        let running_clone = Arc::clone(&self.running);
        let running_clone2 = Arc::clone(&self.running);

        // Take the receiver so we can move it into the async task
        let message_receiver = self
            .message_receiver
            .lock()
            .unwrap()
            .take()
            .expect("NodeEngine already started");

        // Spawn message handler task
        tokio::spawn(async move {
            Self::message_handler_loop(message_receiver, peer_table, config, running_clone).await;
        });

        // Spawn peer maintenance task
        tokio::spawn(async move {
            Self::peer_maintenance_loop(
                peer_table_for_maintenance,
                config_for_maintenance,
                running_clone2,
            )
            .await;
        });

        Ok(())
    }

    /// Stop the network engine.
    pub async fn stop(&self) {
        let mut running = self.running.lock().unwrap();
        *running = false;
    }

    /// Send a message to a specific peer.
    pub fn send_to(&self, peer: &PublicKey, message: NetworkMessage) -> Result<(), NetworkError> {
        // Check if peer exists
        let peer_table = self.peer_table.lock().unwrap();
        if !peer_table.contains_key(peer) {
            return Err(NetworkError::PeerNotFound(peer.to_hex()));
        }

        // In a real implementation, we would serialize and send the message
        // For now, we just pass it through the message channel for local processing
        self.message_sender
            .send(message)
            .map_err(|_| NetworkError::InvalidMessage)?;
        Ok(())
    }

    /// Broadcast a message to all connected peers.
    pub fn broadcast(&self, message: NetworkMessage) -> Result<(), NetworkError> {
        // In a real implementation, we would send to all peers
        // For now, we just pass it through the message channel for local processing
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
        peer_table.insert(peer_info.public_key.clone(), peer_info);
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
    /// Format: `plain://nodes.plain?connect=<ip:port>/<pubkey_hex>`
    pub fn generate_connection_link(&self) -> String {
        let addr = self.config.listen_addr;
        let pubkey_hex = self.config.keypair.public.to_hex();
        format!("plain://nodes.plain?connect={}/{}", addr, pubkey_hex)
    }

    /// Static method to build a connection link from address and public key.
    /// Useful when you don't have a NodeEngine instance (e.g., in CLI tools).
    ///
    /// Format: `plain://nodes.plain?connect=<ip:port>/<pubkey_hex>`
    pub fn build_connection_link(listen_addr: SocketAddr, pubkey: PublicKey) -> String {
        let pubkey_hex = pubkey.to_hex();
        format!("plain://nodes.plain?connect={}/{}", listen_addr, pubkey_hex)
    }

    /// Generate a connection link from a parsed (SocketAddr, PublicKey) tuple.
    /// Useful for roundtrip testing and link reconstruction.
    ///
    /// Format: `plain://nodes.plain?connect=<ip:port>/<pubkey_hex>`
    pub fn generate_connection_link_from_parsed(parsed: (SocketAddr, PublicKey)) -> String {
        Self::build_connection_link(parsed.0, parsed.1)
    }

    /// Parse a connection link URI to extract the address and public key.
    ///
    /// Returns Some((SocketAddr, PublicKey)) if the link is valid.
    pub fn parse_connection_link(link: &str) -> Option<(SocketAddr, PublicKey)> {
        // Strip the "plain://nodes.plain?connect=" prefix
        let prefix = "plain://nodes.plain?connect=";
        if !link.starts_with(prefix) {
            return None;
        }
        let remaining = &link[prefix.len()..];

        // Split into address and pubkey (last 64 hex chars = 32 bytes = Ed25519 pubkey)
        if remaining.len() < 65 {
            // addr + ":" + 64 hex chars minimum
            return None;
        }
        let pubkey_hex = &remaining[remaining.len() - 64..];
        let addr_part = &remaining[..remaining.len() - 64 - 1]; // -1 for the colon

        let addr: SocketAddr = addr_part.parse().ok()?;
        let pubkey_bytes: Vec<u8> = hex::decode(pubkey_hex).ok()?;
        if pubkey_bytes.len() != 32 {
            return None;
        }
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&pubkey_bytes);
        let pubkey = crate::crypto::PublicKey(bytes);

        Some((addr, pubkey))
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

        while *running.lock().unwrap() {
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
                if !pt.contains_key(&sender_pubkey) {
                    // We would normally have the sender's address from the transport layer
                    // For now, we'll use a placeholder
                    let placeholder_addr = SocketAddr::from(([127, 0, 0, 1], 0));
                    pt.insert(
                        sender_pubkey.clone(),
                        PeerInfo {
                            public_key: sender_pubkey,
                            last_seen: Instant::now(),
                            address: placeholder_addr,
                            reputation: 0,
                        },
                    );
                }

                // Add known peers
                for peer_pubkey in known_peers {
                    if !pt.contains_key(&peer_pubkey) {
                        let placeholder_addr = SocketAddr::from(([127, 0, 0, 1], 0));
                        pt.insert(
                            peer_pubkey.clone(),
                            PeerInfo {
                                public_key: peer_pubkey,
                                last_seen: Instant::now(),
                                address: placeholder_addr,
                                reputation: 0,
                            },
                        );
                    }
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
                if !pt.contains_key(&publisher) {
                    let placeholder_addr = SocketAddr::from(([127, 0, 0, 1], 0));
                    pt.insert(
                        publisher.clone(),
                        PeerInfo {
                            public_key: publisher,
                            last_seen: Instant::now(),
                            address: placeholder_addr,
                            reputation: 0,
                        },
                    );
                }

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
}
