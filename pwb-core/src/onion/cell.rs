//! Fixed-size onion cells: construction, peeling, and wire encoding.

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use rand::rngs::OsRng;
use rand::RngCore;
use std::fmt;
use zeroize::{Zeroize, Zeroizing};

use crate::crypto::{DhKeypair, DhPublicKey};

use super::constants::{
    CELL_SIZE, DIRECTIVE_DELIVER, DIRECTIVE_FORWARD, KDF_LAYER_CONTEXT, KDF_PAYLOAD_CONTEXT,
    ONION_VERSION, PAYLOAD_LEN_SIZE, PAYLOAD_MAX, PAYLOAD_PLAINTEXT_SIZE, PAYLOAD_REGION_SIZE,
    SLOTS_SIZE, SLOT_PLAINTEXT_SIZE, SLOT_SIZE, ZERO_NONCE,
};
use super::error::OnionError;
use super::path::validate_path;

/// What a relay should do after successfully peeling its layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnionAction {
    /// Hand [`OnionCell`] `forward` to this relay (the next hop on the path).
    Forward(DhPublicKey),
    /// Accept `payload` as the cell's final destination (exit node).
    Deliver,
}

/// Result of a successful [`OnionCell::peel`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeeledCell {
    /// The directive decrypted from the front layer slot.
    pub action: OnionAction,
    /// For [`OnionAction::Forward`]: the shifted, re-padded cell to hand to
    /// the next hop (still exactly [`CELL_SIZE`] bytes).
    pub forward: Option<OnionCell>,
    /// For [`OnionAction::Deliver`]: the decrypted payload for this node.
    pub payload: Option<Vec<u8>>,
}

/// A fixed-size onion-routed cell.
///
/// Cells are built with [`OnionCell::encapsulate`], transported opaquely,
/// and peeled relay-by-relay with [`OnionCell::peel`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnionCell {
    cell_kind: u8,
    ephemeral_pk: DhPublicKey,
    slots: [u8; SLOTS_SIZE],
    payload_region: [u8; PAYLOAD_REGION_SIZE],
}

impl OnionCell {
    /// Build a fixed-size onion cell that wraps `payload` for delivery to the
    /// final relay of `path` (entry first, exit last).
    ///
    /// The path must satisfy [`validate_path`]: 3..=6 unique relays. The
    /// payload must fit in [`PAYLOAD_MAX`] bytes.
    pub fn encapsulate(payload: &[u8], path: &[DhPublicKey]) -> Result<Self, OnionError> {
        validate_path(path)?;
        if payload.len() > PAYLOAD_MAX {
            return Err(OnionError::PayloadTooLarge { got: payload.len() });
        }
        let n = path.len();

        // Fresh ephemeral keypair per cell: every hop's symmetric keys are
        // single-use, so the AEADs below can safely run with a zero nonce.
        let eph = DhKeypair::generate();
        let mut hop_keys = Vec::with_capacity(n);
        for hop in path {
            let shared = Zeroizing::new(eph.diffie_hellman(hop));
            hop_keys.push(derive_layer_keys(&shared));
        }

        // Slot stack: random fill first (the unused trailing slots become
        // opaque padding), then the real directives written inside-out.
        let mut slots = [0u8; SLOTS_SIZE];
        OsRng.fill_bytes(&mut slots);
        for i in (0..n).rev() {
            let mut directive = [0u8; SLOT_PLAINTEXT_SIZE];
            OsRng.fill_bytes(&mut directive);
            directive[0] = if i + 1 == n {
                DIRECTIVE_DELIVER
            } else {
                DIRECTIVE_FORWARD
            };
            if i + 1 != n {
                directive[1..33].copy_from_slice(&path[i + 1].0);
            }
            let sealed = seal(&hop_keys[i].0, &directive)
                .expect("slot plaintext is fixed-size; ChaCha20-Poly1305 never fails to seal");
            slots[i * SLOT_SIZE..(i + 1) * SLOT_SIZE].copy_from_slice(&sealed);
            directive.zeroize();
        }

        // Payload region: fixed plaintext size with a length prefix and random
        // padding so the ciphertext length never leaks the payload size.
        let mut payload_plain = [0u8; PAYLOAD_PLAINTEXT_SIZE];
        OsRng.fill_bytes(&mut payload_plain);
        payload_plain[..PAYLOAD_LEN_SIZE].copy_from_slice(&(payload.len() as u16).to_le_bytes());
        payload_plain[PAYLOAD_LEN_SIZE..PAYLOAD_LEN_SIZE + payload.len()].copy_from_slice(payload);
        let payload_region: [u8; PAYLOAD_REGION_SIZE] = seal(&hop_keys[n - 1].1, &payload_plain)
            .expect("payload plaintext is fixed-size; ChaCha20-Poly1305 never fails to seal")
            .try_into()
            .expect("sealed payload matches the payload region size");
        payload_plain.zeroize();

        Ok(Self {
            cell_kind: 0,
            ephemeral_pk: eph.public,
            slots,
            payload_region,
        })
    }

    /// Attempt to peel this cell as `relay` (its long-term identity keypair).
    ///
    /// Returns:
    /// - `Ok(Some(peeled))` — the front slot authenticated under the relay's
    ///   layer key; `peeled.action` says whether to forward or deliver.
    /// - `Ok(None)` — the front slot did not authenticate (the cell is not
    ///   addressed to this relay at this position, or it hit a garbage slot);
    ///   the relay should silently drop the cell.
    /// - `Err` — the cell payload failed authentication for the exit node or
    ///   the directive was corrupt (a sender or memory bug, not wire traffic).
    pub fn peel(&self, relay: &DhKeypair) -> Result<Option<PeeledCell>, OnionError> {
        let shared = Zeroizing::new(relay.diffie_hellman(&self.ephemeral_pk));
        let (layer_key, payload_key) = derive_layer_keys(&shared);

        let front = &self.slots[..SLOT_SIZE];
        let Some(directive) = open(&layer_key, front) else {
            return Ok(None);
        };
        let directive = Zeroizing::new(directive);

        match directive[0] {
            DIRECTIVE_FORWARD => {
                let mut next = [0u8; 32];
                next.copy_from_slice(&directive[1..33]);
                let forward = self.shifted_forward_cell();
                Ok(Some(PeeledCell {
                    action: OnionAction::Forward(DhPublicKey(next)),
                    forward: Some(forward),
                    payload: None,
                }))
            }
            DIRECTIVE_DELIVER => {
                let payload_plain =
                    open(&payload_key, &self.payload_region).ok_or(OnionError::MalformedCell)?;
                let len = u16::from_le_bytes([payload_plain[0], payload_plain[1]]) as usize;
                let end = PAYLOAD_LEN_SIZE + len;
                if end > payload_plain.len() {
                    return Err(OnionError::MalformedCell);
                }
                Ok(Some(PeeledCell {
                    action: OnionAction::Deliver,
                    forward: None,
                    payload: Some(payload_plain[PAYLOAD_LEN_SIZE..end].to_vec()),
                }))
            }
            _ => Err(OnionError::MalformedCell),
        }
    }

    /// The cell's wire bytes (exactly [`CELL_SIZE`] bytes).
    pub fn to_bytes(&self) -> [u8; CELL_SIZE] {
        let mut out = [0u8; CELL_SIZE];
        out[0] = ONION_VERSION;
        out[1] = self.cell_kind;
        // bytes 2..8 stay zero (reserved)
        out[8..40].copy_from_slice(&self.ephemeral_pk.0);
        out[40..40 + SLOTS_SIZE].copy_from_slice(&self.slots);
        out[40 + SLOTS_SIZE..].copy_from_slice(&self.payload_region);
        out
    }

    /// Decode a cell from its wire bytes. Rejects wrong sizes and versions.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, OnionError> {
        if bytes.len() != CELL_SIZE {
            return Err(OnionError::MalformedCell);
        }
        if bytes[0] != ONION_VERSION {
            return Err(OnionError::UnsupportedVersion(bytes[0]));
        }
        let mut ephemeral_pk = [0u8; 32];
        ephemeral_pk.copy_from_slice(&bytes[8..40]);
        let mut slots = [0u8; SLOTS_SIZE];
        slots.copy_from_slice(&bytes[40..40 + SLOTS_SIZE]);
        let mut payload_region = [0u8; PAYLOAD_REGION_SIZE];
        payload_region.copy_from_slice(&bytes[40 + SLOTS_SIZE..]);
        Ok(Self {
            cell_kind: bytes[1],
            ephemeral_pk: DhPublicKey(ephemeral_pk),
            slots,
            payload_region,
        })
    }

    /// The cell's routing slots on the wire (opaque 336 bytes).
    pub fn slots(&self) -> &[u8; SLOTS_SIZE] {
        &self.slots
    }

    /// The cell's per-cell ephemeral X25519 public key.
    pub fn ephemeral_pk(&self) -> DhPublicKey {
        self.ephemeral_pk
    }

    /// Shift the slot stack one position left (the front slot was peeled) and
    /// re-randomize the vacated trailing slot so the next hop — and any
    /// observer — sees the same opaque 512-byte shape. The header and payload
    /// region are untouched; the payload stays sealed for the exit node.
    fn shifted_forward_cell(&self) -> Self {
        let mut slots = [0u8; SLOTS_SIZE];
        slots[..SLOTS_SIZE - SLOT_SIZE].copy_from_slice(&self.slots[SLOT_SIZE..]);
        OsRng.fill_bytes(&mut slots[SLOTS_SIZE - SLOT_SIZE..]);
        Self {
            cell_kind: self.cell_kind,
            ephemeral_pk: self.ephemeral_pk,
            slots,
            payload_region: self.payload_region,
        }
    }
}

impl fmt::Display for OnionCell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "OnionCell {{ version: {}, kind: {}, eph: {}, slots: {}B, payload: {}B }}",
            ONION_VERSION,
            self.cell_kind,
            self.ephemeral_pk.to_hex(),
            SLOTS_SIZE,
            PAYLOAD_REGION_SIZE
        )
    }
}

/// Derive the per-hop symmetric sub-keys from an ECDH shared secret. Separate
/// BLAKE3 contexts keep the layer key and payload key independent, so the exit
/// node never reuses one key/nonce pair for both its slot and the payload.
fn derive_layer_keys(shared: &[u8; 32]) -> (Zeroizing<[u8; 32]>, Zeroizing<[u8; 32]>) {
    (
        Zeroizing::new(blake3::derive_key(KDF_LAYER_CONTEXT, shared)),
        Zeroizing::new(blake3::derive_key(KDF_PAYLOAD_CONTEXT, shared)),
    )
}

fn seal(key: &[u8; 32], plaintext: &[u8]) -> Option<Vec<u8>> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    cipher
        .encrypt(Nonce::from_slice(&ZERO_NONCE), plaintext)
        .ok()
}

fn open(key: &[u8; 32], ciphertext: &[u8]) -> Option<Vec<u8>> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    cipher
        .decrypt(Nonce::from_slice(&ZERO_NONCE), ciphertext)
        .ok()
}
