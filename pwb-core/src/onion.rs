//! Layered Sphinx-Style Onion Cells (`pwb-core::onion`).
//!
//! Every onion-routed request travels as a **fixed-size 512-byte cell**. The
//! uniform size (plus fully-encrypted routing slots and payload) means an
//! observer — including the entry relay — cannot learn the query, the packet
//! type, the path length, or the payload length from the wire image.
//!
//! ## Cell layout (512 bytes)
//!
//! ```text
//! offset  size  field
//! 0       1     cell version (ONION_VERSION)
//! 1       1     cell kind (framing only — never a query/packet type)
//! 2       6     reserved (zero; future: circuit id, cover epoch, flags)
//! 8       32    per-cell ephemeral X25519 public key
//! 40      336   layer slots: 6 x 56-byte slots (40-byte directive + 16-byte Poly1305 tag)
//! 376     136   payload region: 120-byte plaintext + 16-byte Poly1305 tag
//! ```
//!
//! ## Layered encryption
//!
//! The sender picks a path of relays (each identified by a long-term X25519
//! [`DhPublicKey`]) and generates one **ephemeral** X25519 keypair for the
//! cell. Hop `i`'s symmetric keys come from the ECDH shared secret between the
//! cell's ephemeral key and relay `i`'s long-term key:
//!
//! ```text
//! shared_i  = X25519(ephemeral_sk, relay_pk_i)
//! layer_i   = BLAKE3-derive_key("pwb-core onion layer v1", shared_i)
//! payload_i = BLAKE3-derive_key("pwb-core onion payload v1", shared_i)
//! ```
//!
//! Only the sender and relay `i` can compute `shared_i`, so no observer can
//! peel any layer. Because every cell carries a **fresh ephemeral key**, each
//! symmetric key encrypts exactly one slot — ChaCha20-Poly1305 therefore runs
//! with an all-zero nonce and never reuses a key/nonce pair.
//!
//! Slot `i` (a 56-byte AEAD blob) holds hop `i`'s directive. Directives are
//! stacked *inside-out*: the last hop's slot decrypts to `DELIVER`, every
//! earlier slot to `FORWARD <next relay pubkey>`. A relay always peels the
//! **front** slot (slot 0) with its layer key; on `FORWARD` it shifts the
//! remaining slots one position left, re-randomizes the vacated trailing slot,
//! and emits the still-512-byte cell. The payload region rides along untouched
//! until the final hop decrypts it with its payload sub-key.
//!
//! Unused trailing slots (path shorter than 6 hops) are random bytes only the
//! sender could produce — they shift forward as opaque padding, so an
//! observer cannot tell where the last real hop sits. Relays drop cells whose
//! front slot fails authentication (`peel` returns `Ok(None)`), which also
//! covers garbage slots and cells addressed to other relays.
//!
//! ## Roadmap hooks
//!
//! - **Randomized hop counts & dummy nodes** (TODO §2.2): [`randomize_path`]
//!   builds every request's path from a caller-supplied active route
//!   (`MIN_HOPS`=3 .. `MAX_LAYER_SLOTS`=6 hops) plus a uniformly random
//!   number of dummy relays. Directives carry no position, length, or
//!   "final hop" flag, so an intermediate relay cannot tell whether its
//!   [`OnionAction::Forward`] names another relay or the final destination
//!   (locked in by
//!   `test_intermediate_cannot_tell_if_next_hop_is_destination`).
//! - **Cover traffic & metadata protection** (TODO §2.3): [`CoverTraffic`]
//!   builds decoy cells over randomized paths (ordinary `cell_kind` 0 cells,
//!   indistinguishable from real requests), [`CoverDispatch`] schedules them
//!   at exponentially distributed, clamped intervals — one per due poll, no
//!   fixed period — and [`crate::net::CellChannel`] carries every cell
//!   hop-by-hop over UDP re-originated from the forwarding relay's own
//!   socket. The datagram envelope is exactly the cell bytes: no
//!   source-address field exists to carry or forge, constant [`CELL_SIZE`]
//!   datagrams hide sizes, and the remaining unencryptable socket metadata
//!   (prev-relay IP → next-relay IP, randomized timing) carries no
//!   actionable signal about the originator.
//! - **Bulk payloads & circuits**: [`PAYLOAD_MAX`] is the per-cell capacity;
//!   `crate::net` fragments larger messages into multiple cells and reassembles
//!   them at the exit, and `NodeEngine::send_onion_request` attaches a return
//!   path so an exit can answer through the circuit.
//!
//! [`DhPublicKey`]: crate::crypto::DhPublicKey

mod cell;
mod constants;
mod cover;
mod error;
mod path;

#[cfg(test)]
mod tests;

pub use cell::{OnionAction, OnionCell, PeeledCell};
pub use constants::{CELL_SIZE, MAX_LAYER_SLOTS, MIN_HOPS, ONION_VERSION, PAYLOAD_MAX};
pub use cover::{CoverConfig, CoverDispatch, CoverTraffic};
pub use error::OnionError;
pub use path::{randomize_path, randomize_path_with, validate_path, RandomizedPath};
