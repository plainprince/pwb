//! Specification tables for byte-mode QR versions 1..=10 at level M.

/// Quiet-zone width (empty modules) rendered around the symbol.
pub const QUIET_ZONE: usize = 4;

/// Highest supported version (level-M byte capacity 213 bytes).
pub const MAX_VERSION: u8 = 10;

/// Error-correction format bits for level M.
pub(super) const ECC_FORMAT_M: u8 = 0b00;

/// `(ec_per_block, g1_blocks, g1_data, g2_blocks, g2_data)` per version 1..=10.
pub(super) const RS_BLOCKS_M: [(u8, u8, u8, u8, u8); 10] = [
    (10, 1, 16, 0, 0),
    (16, 1, 28, 0, 0),
    (26, 1, 44, 0, 0),
    (18, 2, 32, 0, 0),
    (24, 2, 43, 0, 0),
    (16, 4, 27, 0, 0),
    (18, 4, 31, 0, 0),
    (22, 2, 38, 2, 39),
    (22, 3, 36, 2, 37),
    (26, 4, 43, 1, 44),
];

/// Total codewords (data + error correction) per version 1..=10.
pub(super) const TOTAL_CODEWORDS: [usize; 10] = [26, 44, 70, 100, 134, 172, 196, 242, 292, 346];

/// Remainder bits appended after the last codeword, per version 1..=10.
pub(super) const REMAINDER_BITS: [usize; 10] = [0, 7, 7, 7, 7, 7, 0, 0, 0, 0];

/// Alignment-pattern centre coordinates per version 1..=10.
pub(super) const ALIGNMENT_CENTERS: [&[u8]; 10] = [
    &[],
    &[6, 18],
    &[6, 22],
    &[6, 26],
    &[6, 30],
    &[6, 34],
    &[6, 22, 38],
    &[6, 24, 42],
    &[6, 26, 46],
    &[6, 28, 50],
];

/// Pad codeword alternation (0xEC, 0x11) after the terminator.
pub(super) const PAD_CODEWORDS: [u8; 2] = [0xEC, 0x11];

/// GF(256) exponent table, primitive polynomial x^8 + x^4 + x^3 + x^2 + 1
/// (`0x11D`), with the second half mirroring the first for overflow-free
/// multiply-index arithmetic.
pub(super) const GF_EXP: [u8; 512] = {
    let mut t = [0u8; 512];
    let mut x: u32 = 1;
    let mut i = 0;
    while i < 255 {
        t[i] = x as u8;
        x <<= 1;
        if x & 0x100 != 0 {
            x ^= 0x11D;
        }
        i += 1;
    }
    i = 255;
    while i < 512 {
        t[i] = t[i - 255];
        i += 1;
    }
    t
};

/// GF(256) discrete-log table (index by value; entry 0 is unused).
pub(super) const GF_LOG: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 255 {
        t[GF_EXP[i] as usize] = i as u8;
        i += 1;
    }
    t
};

/// Format-info BCH(15,5) generator (x^10 + x^8 + x^5 + x^4 + x^2 + x + 1).
pub(super) const FORMAT_GEN: u16 = 0x537;
/// Format-info mask applied after the BCH remainder is appended.
pub(super) const FORMAT_MASK: u16 = 0x5412;
/// Version-info BCH(18,6) generator.
pub(super) const VERSION_GEN: u16 = 0x1F25;
