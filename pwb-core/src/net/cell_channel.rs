//! Onion-cell UDP transport with source-IP masking (TODO §2.3).

use std::collections::HashMap;
use std::net::SocketAddr;

use tokio::net::UdpSocket;

use crate::crypto::DhPublicKey;
use crate::onion::{OnionCell, CELL_SIZE};

use super::messages::NetworkError;

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
