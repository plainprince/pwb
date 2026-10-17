//! PlainWeb wire format & binary bundle specification (`pwb-core::wire`).
//!
//! A `.pwb` artifact is a single, compact, losslessly compressed binary bundle.
//! It contains:
//! 1. Header (Magic `PWB1`, version, identity key, timestamp).
//! 2. Symbol Table (deduplicated tag IDs, attribute keys, string values).
//! 3. Binary DOM Tree (decompilable back to the exact Plain AST).
//! 4. Optional Script payload (opaque binary blob).

mod bundle;
mod constants;
mod error;
mod symbol;

#[cfg(test)]
mod tests;

pub use bundle::Bundle;
pub use constants::{CURRENT_VERSION, MAGIC};
pub use error::WireError;
pub use symbol::SymbolTable;
