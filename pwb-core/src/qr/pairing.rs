//! Pairing-payload parsing for scanned connection links.

use std::net::SocketAddr;

use crate::crypto::PublicKey;
use crate::system;

/// Parse a scanned pairing payload into `(SocketAddr, PublicKey)`.
///
/// Accepts the canonical `plain://nodes.plain?connect=<ip:port>/<hex>`
/// connection link as well as the compact `<ip:port>/<hex>` form a QR code
/// may carry to save modules. Returns `None` for anything else.
pub fn parse_pairing_payload(payload: &str) -> Option<(SocketAddr, PublicKey)> {
    let payload = payload.trim();
    if let Some(target) = system::parse_connection_link(payload) {
        return Some(target);
    }
    let (addr, key_hex) = payload.rsplit_once('/')?;
    let addr: SocketAddr = addr.parse().ok()?;
    let bytes = hex::decode(key_hex).ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&bytes);
    Some((addr, PublicKey(key)))
}
