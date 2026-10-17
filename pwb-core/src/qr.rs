//! QR code encoding & pairing-payload parsing (§3.3).
//!
//! Byte-mode QR versions 1..=10 at error-correction level M — enough for the
//! longest `plain://nodes.plain?connect=...` connection link (level-M byte
//! capacity tops out at 213 bytes on version 10). Encoding is implemented
//! directly from the specification tables (Reed–Solomon block layouts,
//! alignment centres, BCH format/version codes, the eight data masks, and
//! penalty scoring) with no new dependencies.
//!
//! Pairing flow: `QrCode::encode_connection_link` renders a node's link as a
//! binary generator (`bitmap` / `to_svg` / `to_ascii`); a scanner hands the
//! decoded string to `parse_pairing_payload`, which returns the
//! `(SocketAddr, PublicKey)` pair needed by `NodeEngine::connect_to`.

mod code;
mod constants;
mod error;
mod gf;
mod matrix;
mod pairing;

#[cfg(test)]
mod tests;

pub use code::QrCode;
pub use constants::{MAX_VERSION, QUIET_ZONE};
pub use error::QrError;
pub use pairing::parse_pairing_payload;
