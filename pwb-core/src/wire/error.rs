//! Errors raised while decoding a PlainWeb bundle.

#[derive(Debug, Clone, PartialEq)]
pub enum WireError {
    InvalidMagic,
    UnsupportedVersion(u16),
    UnexpectedEof,
    InvalidTag(u8),
    InvalidSymbol(u16),
    CorruptedData(String),
    /// The compressed `.pwb` envelope is malformed or failed to decompress.
    CompressionFailed(String),
    /// Extra bytes after a complete bundle (or after the zlib stream in a
    /// compressed container). The content ID covers the exact bytes, so
    /// trailing garbage must be rejected rather than silently ignored.
    TrailingBytes(usize),
}
