//! PlainWeb wire format & binary bundle specification (`pwb-core::wire`).
//!
//! A `.pwb` artifact is a single, compact, losslessly compressed binary bundle.
//! It contains:
//! 1. Header (Magic `PWB1`, version, identity key, timestamp).
//! 2. Symbol Table (deduplicated tag IDs, attribute keys, string values).
//! 3. Binary DOM Tree (decompilable back to the exact Plain AST).
//! 4. Optional Script payload (opaque binary blob).
//!
//! Artifacts are stored in the `PWBZ` container: magic + compression version +
//! uncompressed length + BLAKE3 digest + zlib stream of the `PWB1` bytes.
//! [`Bundle::deserialize`] accepts both, so legacy artifacts keep loading.

mod bundle;
mod constants;
mod error;
mod symbol;

#[cfg(test)]
mod tests;

pub use bundle::Bundle;
pub use constants::{
    COMPRESSED_MAGIC, COMPRESSION_VERSION, CURRENT_VERSION, MAGIC, MAX_DECOMPRESSED_BYTES,
};
pub use error::WireError;
pub use symbol::SymbolTable;
