//! The encoded QR symbol and its renderers.

use std::fmt::Write as _;
use std::net::SocketAddr;

use crate::crypto::PublicKey;
use crate::system;

use super::constants::{MAX_VERSION, QUIET_ZONE};
use super::error::QrError;
use super::gf::{build_codewords, max_byte_payload};
use super::matrix::assemble;

/// An encoded QR symbol: byte mode, error-correction level M, version 1..=10.
///
/// `modules` stores dark/light cells row-major without the quiet zone; the
/// quiet zone is added by [`QrCode::bitmap`] and the renderers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QrCode {
    pub(super) version: u8,
    pub(super) mask: u8,
    pub(super) size: usize,
    pub(super) modules: Vec<bool>,
}

impl QrCode {
    /// Encode a UTF-8 payload (its byte length selects the version).
    pub fn encode(payload: &str) -> Result<Self, QrError> {
        Self::encode_bytes(payload.as_bytes())
    }

    /// Encode raw bytes in byte mode, choosing the smallest version 1..=10
    /// whose level-M capacity fits the payload.
    pub fn encode_bytes(data: &[u8]) -> Result<Self, QrError> {
        let version = (1..=MAX_VERSION as usize)
            .find(|&v| data.len() <= max_byte_payload(v))
            .ok_or(QrError::PayloadTooLong {
                len: data.len(),
                max: max_byte_payload(MAX_VERSION as usize),
            })?;
        let codewords = build_codewords(data, version);
        Ok(assemble(version, &codewords))
    }

    /// Encode the canonical `plain://nodes.plain?connect=...` link for a peer.
    pub fn encode_connection_link(
        listen_addr: SocketAddr,
        pubkey: PublicKey,
    ) -> Result<Self, QrError> {
        Self::encode(&system::build_connection_link(listen_addr, pubkey))
    }

    /// Symbol version (1..=10) chosen for the payload.
    pub fn version(&self) -> u8 {
        self.version
    }

    /// Data mask pattern (0..=7) selected by penalty scoring.
    pub fn mask(&self) -> u8 {
        self.mask
    }

    /// Module width/height of the symbol (without the quiet zone).
    pub fn size(&self) -> usize {
        self.size
    }

    /// Dark (`true`) / light (`false`) state of one module.
    pub fn module(&self, row: usize, col: usize) -> bool {
        self.modules[row * self.size + col]
    }

    /// Binary generator output: row-major dark/light cells including the
    /// [`QUIET_ZONE`] border on every side.
    pub fn bitmap(&self) -> Vec<Vec<bool>> {
        let width = self.size + QUIET_ZONE * 2;
        let mut rows = vec![vec![false; width]; QUIET_ZONE];
        for r in 0..self.size {
            let mut row = vec![false; QUIET_ZONE];
            row.extend(
                self.modules[r * self.size..(r + 1) * self.size]
                    .iter()
                    .copied(),
            );
            row.extend([false; QUIET_ZONE]);
            rows.push(row);
        }
        rows.extend(vec![vec![false; width]; QUIET_ZONE]);
        rows
    }

    /// Compact SVG rendering of the symbol plus quiet zone.
    pub fn to_svg(&self) -> String {
        let bitmap = self.bitmap();
        let n = bitmap.len();
        let mut svg = String::new();
        let _ = write!(
            svg,
            "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {n} {n}\" shape-rendering=\"crispEdges\">"
        );
        let _ = write!(svg, "<path fill=\"#fff\" d=\"M0 0h{n}v{n}H0z\"/>");
        svg.push_str("<path fill=\"#000\" d=\"");
        for (y, row) in bitmap.iter().enumerate() {
            for (x, &dark) in row.iter().enumerate() {
                if dark {
                    let _ = write!(svg, "M{x} {y}h1v1h-1z");
                }
            }
        }
        svg.push_str("\"/></svg>");
        svg
    }

    /// Terminal rendering using double-width block characters.
    pub fn to_ascii(&self) -> String {
        self.bitmap()
            .iter()
            .map(|row| {
                row.iter()
                    .map(|&dark| if dark { "██" } else { "  " })
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}
