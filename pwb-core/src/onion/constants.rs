//! Onion-cell wire geometry and key-derivation constants.

/// Cell format version written into every cell's first byte.
pub const ONION_VERSION: u8 = 1;

/// Fixed onion cell size in bytes. 512 stays far below any IP/UDP MTU (no
/// fragmentation) and makes traffic analysis of query sizes impossible.
pub const CELL_SIZE: usize = 512;

/// Maximum number of layer slots (and therefore onion hops) per cell.
pub const MAX_LAYER_SLOTS: usize = 6;

/// Minimum number of relays on a built path (entry .. exit inclusive).
pub const MIN_HOPS: usize = 3;

/// Maximum payload bytes that fit in one cell (see module docs for the
/// payload-region math: 136 - 16-byte tag - 2-byte length prefix).
pub const PAYLOAD_MAX: usize = 118;

pub(super) const SLOT_SIZE: usize = 56;
pub(super) const SLOT_TAG_SIZE: usize = 16;
pub(super) const SLOT_PLAINTEXT_SIZE: usize = SLOT_SIZE - SLOT_TAG_SIZE;
pub(super) const HEADER_SIZE: usize = 40;
pub(super) const SLOTS_SIZE: usize = MAX_LAYER_SLOTS * SLOT_SIZE;
pub(super) const PAYLOAD_REGION_SIZE: usize = CELL_SIZE - HEADER_SIZE - SLOTS_SIZE;
pub(super) const PAYLOAD_TAG_SIZE: usize = 16;
pub(super) const PAYLOAD_LEN_SIZE: usize = 2;
pub(super) const PAYLOAD_PLAINTEXT_SIZE: usize = PAYLOAD_REGION_SIZE - PAYLOAD_TAG_SIZE;

pub(super) const KDF_LAYER_CONTEXT: &str = "pwb-core onion layer v1";
pub(super) const KDF_PAYLOAD_CONTEXT: &str = "pwb-core onion payload v1";

pub(super) const DIRECTIVE_FORWARD: u8 = 0x01;
pub(super) const DIRECTIVE_DELIVER: u8 = 0x02;

pub(super) const ZERO_NONCE: [u8; 12] = [0u8; 12];
