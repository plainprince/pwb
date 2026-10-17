//! Errors building or decoding onion cells.

use std::fmt;

use super::constants::{MAX_LAYER_SLOTS, MIN_HOPS, PAYLOAD_MAX};

/// Errors building or decoding onion cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnionError {
    PayloadTooLarge { got: usize },
    PathTooShort { got: usize },
    PathTooLong { got: usize },
    DuplicateRelay,
    MalformedCell,
    UnsupportedVersion(u8),
}

impl fmt::Display for OnionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OnionError::PayloadTooLarge { got } => write!(
                f,
                "payload of {got} bytes exceeds the per-cell onion capacity of {PAYLOAD_MAX} bytes"
            ),
            OnionError::PathTooShort { got } => write!(
                f,
                "onion path must include at least {MIN_HOPS} relays, got {got}"
            ),
            OnionError::PathTooLong { got } => write!(
                f,
                "onion path exceeds the {MAX_LAYER_SLOTS}-slot cell capacity, got {got}"
            ),
            OnionError::DuplicateRelay => write!(f, "onion path contains a duplicated relay"),
            OnionError::MalformedCell => write!(f, "malformed onion cell"),
            OnionError::UnsupportedVersion(v) => write!(f, "unsupported onion version {v}"),
        }
    }
}

impl std::error::Error for OnionError {}
