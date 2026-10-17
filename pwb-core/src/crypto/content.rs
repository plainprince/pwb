//! BLAKE3 content addressing.

use serde::{Deserialize, Serialize};
use std::fmt;

/// 256-bit Content Digest / Hash (BLAKE3/SHA256).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContentId(pub [u8; 32]);

impl ContentId {
    pub fn compute(data: &[u8]) -> Self {
        let hash = blake3::hash(data);
        ContentId(*hash.as_bytes())
    }

    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{:02x}", b)).collect()
    }
}

impl fmt::Display for ContentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}
