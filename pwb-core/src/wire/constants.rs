//! Wire-format magic and version constants.

pub const MAGIC: &[u8; 4] = b"PWB1";
/// v2 adds the component registry and the named script-module map (bundling).
pub const CURRENT_VERSION: u16 = 2;
