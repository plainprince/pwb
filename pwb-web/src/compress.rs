//! Lossless compression & binary encoding (`pwb-web::compress`).
//!
//! A `.pwb` artifact is stored in the compressed `PWBZ` container: a 41-byte
//! header (magic, compression version, uncompressed length, BLAKE3 digest)
//! followed by a zlib stream of the canonical `PWB1` bundle. Decompression lives in
//! [`pwb_core::wire::Bundle::deserialize`], which also accepts legacy
//! uncompressed `PWB1` bytes, so artifacts made before compression still load.

use pwb_core::wire::{Bundle, WireError};

/// Compress a `Bundle` into binary `.pwb` bytes (`PWBZ` container).
pub fn compress_bundle(bundle: &Bundle) -> Vec<u8> {
    bundle.serialize_compressed()
}

/// Decompress and decode `.pwb` bytes, accepting both `PWBZ` and legacy
/// uncompressed `PWB1` artifacts.
pub fn decompress_bundle(bytes: &[u8]) -> Result<Bundle, WireError> {
    Bundle::deserialize(bytes)
}

/// Whether `bytes` is a compressed `PWBZ` artifact.
pub fn is_compressed(bytes: &[u8]) -> bool {
    Bundle::is_compressed(bytes)
}
