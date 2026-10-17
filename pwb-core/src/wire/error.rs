//! Errors raised while decoding a PlainWeb bundle.

#[derive(Debug, Clone, PartialEq)]
pub enum WireError {
    InvalidMagic,
    UnsupportedVersion(u16),
    UnexpectedEof,
    InvalidTag(u8),
    InvalidSymbol(u16),
    CorruptedData(String),
}
