//! 256-bit Kademlia node identifiers and the XOR metric.

use crate::crypto::PublicKey;

use super::constants::ID_BITS;

/// A 256-bit Kademlia node ID: BLAKE3 of the node's Ed25519 public key.
///
/// Distances are plain XOR of the big-endian bit strings; lexicographic byte
/// order equals numeric order, so `Ord` on the raw bytes *is* distance order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId([u8; 32]);

impl NodeId {
    /// Derive the routing-table ID for a node identity.
    pub fn from_public_key(key: &PublicKey) -> Self {
        NodeId(*blake3::hash(&key.0).as_bytes())
    }

    /// Wrap raw ID bytes (tests, wire format).
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        NodeId(bytes)
    }

    /// Raw big-endian bytes of the ID.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// XOR distance between two IDs (the Kademlia metric).
    pub fn xor(&self, other: &NodeId) -> [u8; 32] {
        let mut d = [0u8; 32];
        for (i, (a, b)) in self.0.iter().zip(other.0.iter()).enumerate() {
            d[i] = a ^ b;
        }
        d
    }

    /// Length of the shared bit prefix (0..=256); 256 only for equal IDs.
    pub fn common_prefix_len(&self, other: &NodeId) -> usize {
        let mut i = 0;
        while i < ID_BITS {
            let byte = i / 8;
            let bit = i % 8;
            if self.0[byte] & (0x80 >> bit) != other.0[byte] & (0x80 >> bit) {
                break;
            }
            i += 1;
        }
        i
    }
}
