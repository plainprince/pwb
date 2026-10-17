//! Discovery beacon and socket error types.

use std::fmt;

use thiserror::Error;

use super::constants::BEACON_SIZE;

/// Errors decoding or validating a discovery beacon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BeaconError {
    /// Datagram was not exactly [`BEACON_SIZE`] bytes.
    BadSize { got: usize },
    /// Missing the `PWBD` magic.
    BadMagic,
    /// Unknown beacon format version.
    UnsupportedVersion(u8),
    /// Beacon timestamp is older than the freshness window.
    Stale { age_secs: u64, max_age_secs: u64 },
    /// Beacon timestamp is ahead of the local clock (flood/replay attempt).
    FromFuture { skew_secs: u64, max_age_secs: u64 },
    /// Signature does not verify for the embedded public key.
    BadSignature,
}

impl fmt::Display for BeaconError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BeaconError::BadSize { got } => {
                write!(f, "beacon must be exactly {BEACON_SIZE} bytes, got {got}")
            }
            BeaconError::BadMagic => write!(f, "not a PlainWeb discovery beacon (bad magic)"),
            BeaconError::UnsupportedVersion(v) => write!(f, "unsupported beacon version {v}"),
            BeaconError::Stale {
                age_secs,
                max_age_secs,
            } => write!(
                f,
                "beacon is {age_secs}s old (max {max_age_secs}s freshness window)"
            ),
            BeaconError::FromFuture {
                skew_secs,
                max_age_secs,
            } => write!(
                f,
                "beacon is {skew_secs}s in the future (max {max_age_secs}s clock skew)"
            ),
            BeaconError::BadSignature => write!(f, "beacon signature does not verify"),
        }
    }
}

impl std::error::Error for BeaconError {}

/// Errors from discovery socket operations.
#[derive(Debug, Error)]
pub enum DiscoveryError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Beacon(#[from] BeaconError),
}
