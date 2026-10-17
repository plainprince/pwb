//! GF(256) arithmetic, Reed–Solomon blocks, and the codeword bitstream.

use super::constants::{GF_EXP, GF_LOG, PAD_CODEWORDS, RS_BLOCKS_M, TOTAL_CODEWORDS};

/// Data-codeword capacity (per spec block table) of one version at level M.
pub(super) fn data_codewords(version: usize) -> usize {
    let (_, g1_blocks, g1_data, g2_blocks, g2_data) = RS_BLOCKS_M[version - 1];
    g1_blocks as usize * g1_data as usize + g2_blocks as usize * g2_data as usize
}

/// Largest byte-mode payload (4-bit mode + count + bytes) for a version.
pub(super) fn max_byte_payload(version: usize) -> usize {
    let count_bits = if version <= 9 { 8 } else { 16 };
    (data_codewords(version) * 8 - 4 - count_bits) / 8
}

/// Multiply in GF(256) via table lookups.
fn gf_mul(a: u8, b: u8) -> u8 {
    if a == 0 || b == 0 {
        0
    } else {
        GF_EXP[GF_LOG[a as usize] as usize + GF_LOG[b as usize] as usize]
    }
}

/// Reed–Solomon generator polynomial of `degree`, highest degree first.
pub(super) fn rs_generator(degree: usize) -> Vec<u8> {
    let mut poly = vec![1u8];
    for &alpha in &GF_EXP[..degree] {
        let mut next = vec![0u8; poly.len() + 1];
        for (idx, &coef) in poly.iter().enumerate() {
            next[idx] ^= coef;
            next[idx + 1] ^= gf_mul(coef, alpha);
        }
        poly = next;
    }
    poly
}

/// Reed–Solomon remainder of `data` divided by the generator (the EC block).
pub(super) fn rs_remainder(data: &[u8], generator: &[u8]) -> Vec<u8> {
    let degree = generator.len() - 1;
    let mut buf = data.to_vec();
    buf.resize(data.len() + degree, 0);
    for i in 0..data.len() {
        let coef = buf[i];
        if coef != 0 {
            for (j, &g) in generator.iter().enumerate() {
                buf[i + j] ^= gf_mul(g, coef);
            }
        }
    }
    buf[data.len()..].to_vec()
}

/// Push `count` bits of `value` (most significant first) onto a bit buffer.
fn push_bits(bits: &mut Vec<bool>, value: u32, count: usize) {
    for k in (0..count).rev() {
        bits.push(value & (1 << k) != 0);
    }
}

/// Build the full interleaved codeword stream (data blocks + RS blocks).
pub(super) fn build_codewords(data: &[u8], version: usize) -> Vec<u8> {
    let (ec_len, g1_blocks, g1_data, g2_blocks, g2_data) = RS_BLOCKS_M[version - 1];
    let data_len = g1_blocks as usize * g1_data as usize + g2_blocks as usize * g2_data as usize;

    let mut bits: Vec<bool> = Vec::with_capacity(data_len * 8);
    push_bits(&mut bits, 0b0100, 4); // byte mode
    let count_bits = if version <= 9 { 8 } else { 16 };
    push_bits(&mut bits, data.len() as u32, count_bits);
    for &b in data {
        push_bits(&mut bits, b as u32, 8);
    }

    // Terminator (up to four zero bits), then pad to a byte boundary.
    let cap = data_len * 8;
    let terminator = (cap - bits.len()).min(4);
    push_bits(&mut bits, 0, terminator);
    while !bits.len().is_multiple_of(8) {
        bits.push(false);
    }

    let mut codewords: Vec<u8> = bits
        .chunks(8)
        .map(|byte| {
            byte.iter()
                .fold(0u8, |acc, &bit| (acc << 1) | u8::from(bit))
        })
        .collect();
    let mut pad = 0;
    while codewords.len() < data_len {
        codewords.push(PAD_CODEWORDS[pad % 2]);
        pad += 1;
    }

    // Split into blocks, append RS remainders, and interleave both.
    let lens: Vec<usize> = (0..g1_blocks)
        .map(|_| g1_data as usize)
        .chain((0..g2_blocks).map(|_| g2_data as usize))
        .collect();
    let generator = rs_generator(ec_len as usize);
    let mut blocks: Vec<Vec<u8>> = Vec::with_capacity(lens.len());
    let mut offset = 0;
    for &len in &lens {
        blocks.push(codewords[offset..offset + len].to_vec());
        offset += len;
    }
    let ec_blocks: Vec<Vec<u8>> = blocks
        .iter()
        .map(|block| rs_remainder(block, &generator))
        .collect();

    let mut out: Vec<u8> = Vec::with_capacity(TOTAL_CODEWORDS[version - 1]);
    let max_data = lens.iter().copied().max().unwrap_or(0);
    for i in 0..max_data {
        for block in &blocks {
            if i < block.len() {
                out.push(block[i]);
            }
        }
    }
    for i in 0..ec_len as usize {
        for block in &ec_blocks {
            out.push(block[i]);
        }
    }
    debug_assert_eq!(out.len(), TOTAL_CODEWORDS[version - 1]);
    out
}
