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
    /// The bundle names an author but does not carry a valid signature by that
    /// author. A named author is never accepted on trust.
    InvalidAuthorSignature,
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WireError::InvalidMagic => write!(f, "not a PlainWeb artifact (bad magic)"),
            WireError::UnsupportedVersion(version) => {
                write!(f, "unsupported PlainWeb bundle version {version}")
            }
            WireError::UnexpectedEof => write!(f, "the artifact is truncated"),
            WireError::InvalidTag(tag) => write!(f, "invalid element tag {tag}"),
            WireError::InvalidSymbol(symbol) => write!(f, "invalid string reference {symbol}"),
            WireError::CorruptedData(message) => write!(f, "corrupted bundle data: {message}"),
            WireError::CompressionFailed(message) => {
                write!(f, "the compressed artifact is invalid: {message}")
            }
            WireError::TrailingBytes(bytes) => {
                write!(f, "the artifact has {bytes} unexpected trailing byte(s)")
            }
            WireError::InvalidAuthorSignature => {
                write!(f, "the author signature is missing or invalid")
            }
        }
    }
}

impl std::error::Error for WireError {}
