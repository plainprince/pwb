//! NodeEngine background loops: inbound receiver, outbound sender, message
//! handler, peer maintenance, and per-message processing.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::interval;

use crate::crypto::PublicKey;

use super::engine::{NodeConfig, PeerInfo};
use super::messages::{NetworkMessage, OutgoingDatagram};

/// Internal message handler loop.
pub(super) async fn message_handler_loop(
    mut receiver: mpsc::UnboundedReceiver<(SocketAddr, NetworkMessage)>,
    peer_table: Arc<Mutex<HashMap<PublicKey, PeerInfo>>>,
    config: NodeConfig,
    running: Arc<Mutex<bool>>,
) {
    while *running.lock().unwrap() {
        tokio::select! {
            Some((sender_addr, message)) = receiver.recv() => {
                // Process incoming message, tagged with the sender's real address.
                process_message(message, sender_addr, &peer_table, &config).await;
            }
            _ = tokio::time::sleep(Duration::from_millis(10)) => {
                // Small sleep to prevent busy loop
            }
        }
    }
}

/// Internal peer maintenance loop.
pub(super) async fn peer_maintenance_loop(
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
pub(super) async fn sender_loop(
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
pub(super) async fn receiver_loop(
    socket: Arc<UdpSocket>,
    sender: mpsc::UnboundedSender<(SocketAddr, NetworkMessage)>,
    running: Arc<Mutex<bool>>,
) {
    let mut buf = [0u8; 65535];
    while *running.lock().unwrap() {
        tokio::select! {
            result = socket.recv_from(&mut buf) => {
                match result {
                    Ok((len, from)) => {
                        if let Ok(message) = serde_json::from_slice::<NetworkMessage>(&buf[..len]) {
                            let _ = sender.send((from, message));
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
///
/// `sender_addr` is the real UDP source address of the datagram; it is used
/// (instead of a placeholder) so the receiving node can reply to the peer.
async fn process_message(
    message: NetworkMessage,
    sender_addr: SocketAddr,
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
            // Add sender to peer table if not present, using the real
            // source address observed on the wire.
            let mut pt = peer_table.lock().unwrap();
            pt.entry(sender_pubkey).or_insert_with(|| PeerInfo {
                public_key: sender_pubkey,
                last_seen: Instant::now(),
                address: sender_addr,
                reputation: 0,
            });

            // Add known peers
            for peer_pubkey in known_peers {
                pt.entry(peer_pubkey).or_insert_with(|| PeerInfo {
                    public_key: peer_pubkey,
                    last_seen: Instant::now(),
                    address: sender_addr,
                    reputation: 0,
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

            // Add the publisher to the peer table if we don't know them yet,
            // using the real source address observed on the wire.
            pt.entry(publisher).or_insert_with(|| PeerInfo {
                public_key: publisher,
                last_seen: Instant::now(),
                address: sender_addr,
                reputation: 0,
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
