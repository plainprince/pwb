//! Wire-format magic and version constants.

pub const MAGIC: &[u8; 4] = b"PWB1";
/// v2 adds the component registry and the named script-module map (bundling).
pub const CURRENT_VERSION: u16 = 2;

/// Magic for the compressed `.pwb` container (`PWBZ`). Uncompressed `PWB1`
/// artifacts remain readable, so the compression layer is backwards compatible.
pub const COMPRESSED_MAGIC: &[u8; 4] = b"PWBZ";
/// Version of the compression envelope (zlib-deflate payload).
pub const COMPRESSION_VERSION: u8 = 1;
/// Upper bound on a decompressed bundle, guarding against decompression bombs
/// from a hostile artifact or peer.
pub const MAX_DECOMPRESSED_BYTES: usize = 64 * 1024 * 1024;
