//! Errors produced while encoding a QR symbol.

use std::fmt;

use super::constants::MAX_VERSION;

/// Error produced when a payload cannot fit any supported QR version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QrError {
    /// The payload exceeds the level-M version-10 byte capacity.
    PayloadTooLong { len: usize, max: usize },
}

impl fmt::Display for QrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            QrError::PayloadTooLong { len, max } => write!(
                f,
                "QR payload of {len} bytes exceeds level-M version-{MAX_VERSION} capacity of {max} bytes"
            ),
        }
    }
}

impl std::error::Error for QrError {}
