//! QR code encoding & pairing-payload parsing (§3.3).
//!
//! Byte-mode QR versions 1..=10 at error-correction level M — enough for the
//! longest `plain://nodes.plain?connect=...` connection link (level-M byte
//! capacity tops out at 213 bytes on version 10). Encoding is implemented
//! directly from the specification tables (Reed–Solomon block layouts,
//! alignment centres, BCH format/version codes, the eight data masks, and
//! penalty scoring) with no new dependencies.
//!
//! Pairing flow: `QrCode::encode_connection_link` renders a node's link as a
//! binary generator (`bitmap` / `to_svg` / `to_ascii`); a scanner hands the
//! decoded string to `parse_pairing_payload`, which returns the
//! `(SocketAddr, PublicKey)` pair needed by `NodeEngine::connect_to`.

use std::fmt;
use std::fmt::Write as _;
use std::net::SocketAddr;

use crate::crypto::PublicKey;
use crate::system;

/// Quiet-zone width (empty modules) rendered around the symbol.
pub const QUIET_ZONE: usize = 4;

/// Highest supported version (level-M byte capacity 213 bytes).
pub const MAX_VERSION: u8 = 10;

/// Error-correction format bits for level M.
const ECC_FORMAT_M: u8 = 0b00;

/// `(ec_per_block, g1_blocks, g1_data, g2_blocks, g2_data)` per version 1..=10.
const RS_BLOCKS_M: [(u8, u8, u8, u8, u8); 10] = [
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
const TOTAL_CODEWORDS: [usize; 10] = [26, 44, 70, 100, 134, 172, 196, 242, 292, 346];

/// Remainder bits appended after the last codeword, per version 1..=10.
const REMAINDER_BITS: [usize; 10] = [0, 7, 7, 7, 7, 7, 0, 0, 0, 0];

/// Alignment-pattern centre coordinates per version 1..=10.
const ALIGNMENT_CENTERS: [&[u8]; 10] = [
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
const PAD_CODEWORDS: [u8; 2] = [0xEC, 0x11];

/// GF(256) exponent table, primitive polynomial x^8 + x^4 + x^3 + x^2 + 1
/// (`0x11D`), with the second half mirroring the first for overflow-free
/// multiply-index arithmetic.
const GF_EXP: [u8; 512] = {
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
const GF_LOG: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 255 {
        t[GF_EXP[i] as usize] = i as u8;
        i += 1;
    }
    t
};

/// Format-info BCH(15,5) generator (x^10 + x^8 + x^5 + x^4 + x^2 + x + 1).
const FORMAT_GEN: u16 = 0x537;
/// Format-info mask applied after the BCH remainder is appended.
const FORMAT_MASK: u16 = 0x5412;
/// Version-info BCH(18,6) generator.
const VERSION_GEN: u16 = 0x1F25;

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

/// An encoded QR symbol: byte mode, error-correction level M, version 1..=10.
///
/// `modules` stores dark/light cells row-major without the quiet zone; the
/// quiet zone is added by [`QrCode::bitmap`] and the renderers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QrCode {
    version: u8,
    mask: u8,
    size: usize,
    modules: Vec<bool>,
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

/// Parse a scanned pairing payload into `(SocketAddr, PublicKey)`.
///
/// Accepts the canonical `plain://nodes.plain?connect=<ip:port>/<hex>`
/// connection link as well as the compact `<ip:port>/<hex>` form a QR code
/// may carry to save modules. Returns `None` for anything else.
pub fn parse_pairing_payload(payload: &str) -> Option<(SocketAddr, PublicKey)> {
    let payload = payload.trim();
    if let Some(target) = system::parse_connection_link(payload) {
        return Some(target);
    }
    let (addr, key_hex) = payload.rsplit_once('/')?;
    let addr: SocketAddr = addr.parse().ok()?;
    let bytes = hex::decode(key_hex).ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&bytes);
    Some((addr, PublicKey(key)))
}

/// Data-codeword capacity (per spec block table) of one version at level M.
fn data_codewords(version: usize) -> usize {
    let (_, g1_blocks, g1_data, g2_blocks, g2_data) = RS_BLOCKS_M[version - 1];
    g1_blocks as usize * g1_data as usize + g2_blocks as usize * g2_data as usize
}

/// Largest byte-mode payload (4-bit mode + count + bytes) for a version.
fn max_byte_payload(version: usize) -> usize {
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
fn rs_generator(degree: usize) -> Vec<u8> {
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
fn rs_remainder(data: &[u8], generator: &[u8]) -> Vec<u8> {
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
fn build_codewords(data: &[u8], version: usize) -> Vec<u8> {
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

/// The QR symbol grid: dark modules plus a parallel function-module mask.
struct Matrix {
    size: usize,
    dark: Vec<bool>,
    func: Vec<bool>,
}

impl Matrix {
    fn new(version: usize) -> Self {
        let size = version * 4 + 17;
        let cells = size * size;
        Matrix {
            size,
            dark: vec![false; cells],
            func: vec![false; cells],
        }
    }

    fn set_fn(&mut self, row: usize, col: usize, dark: bool) {
        let idx = row * self.size + col;
        self.dark[idx] = dark;
        self.func[idx] = true;
    }

    fn get(&self, row: usize, col: usize) -> bool {
        self.dark[row * self.size + col]
    }
}

/// Place the timing rows/columns, finder patterns + separators, and alignment
/// patterns through `set(row, col, dark)`. Shared by the encoder and by the
/// test-only decoder's function-mask rebuild.
fn place_geometry(size: usize, version: usize, mut set: impl FnMut(usize, usize, bool)) {
    // Horizontal and vertical timing patterns (finders overwrite their ends).
    for i in 0..size {
        set(6, i, i % 2 == 0);
        set(i, 6, i % 2 == 0);
    }

    // Three finder patterns: 7x7 rings plus their one-module separators.
    let s = size as isize;
    for &(center_r, center_c) in &[(3, 3), (3, s - 4), (s - 4, 3)] {
        for dr in -4..=4 {
            for dc in -4..=4 {
                let (r, c) = (center_r + dr, center_c + dc);
                if r >= 0 && r < s && c >= 0 && c < s {
                    let cheb = dr.abs().max(dc.abs());
                    set(r as usize, c as usize, cheb != 2 && cheb != 4);
                }
            }
        }
    }

    // Alignment patterns (the three finder-adjacent corners are omitted).
    let centers = ALIGNMENT_CENTERS[version - 1];
    if centers.is_empty() {
        return;
    }
    let last = centers.len() - 1;
    for (ri, &row) in centers.iter().enumerate() {
        for (ci, &col) in centers.iter().enumerate() {
            if (ri == 0 && ci == 0) || (ri == 0 && ci == last) || (ri == last && ci == 0) {
                continue;
            }
            for dr in -2isize..=2 {
                for dc in -2isize..=2 {
                    let cheb = dr.abs().max(dc.abs());
                    set(
                        (row as isize + dr) as usize,
                        (col as isize + dc) as usize,
                        cheb != 1,
                    );
                }
            }
        }
    }
}

/// Visit every module of format-info copy 0 or 1 as `(row, col, bit_index)`.
fn for_each_format_cell(size: usize, copy: u8, mut f: impl FnMut(usize, usize, usize)) {
    if copy == 0 {
        for i in 0..6 {
            f(i, 8, i);
        }
        f(7, 8, 6);
        f(8, 8, 7);
        f(8, 7, 8);
        for i in 9..15 {
            f(8, 14 - i, i);
        }
    } else {
        for i in 0..8 {
            f(8, size - 1 - i, i);
        }
        for i in 8..15 {
            f(size - 15 + i, 8, i);
        }
    }
}

/// Visit both copies of the version-information block (versions 7..=10 only)
/// as `(row, col, bit_index)`.
fn for_each_version_cell(version: usize, size: usize, mut f: impl FnMut(usize, usize, usize)) {
    if version < 7 {
        return;
    }
    for i in 0..18 {
        let a = size - 11 + i % 3;
        let b = i / 3;
        f(b, a, i);
        f(a, b, i);
    }
}

/// BCH(15,5) format bits for level M + one mask pattern (15 bits, LSB-first
/// bit `i` lands on format cell `i`).
fn format_bits(mask: u8) -> u16 {
    let data = u16::from(ECC_FORMAT_M) << 3 | u16::from(mask);
    let mut rem = data;
    for _ in 0..10 {
        rem = (rem << 1) ^ (if rem & 0x200 != 0 { FORMAT_GEN } else { 0 });
    }
    ((data << 10) | rem) ^ FORMAT_MASK
}

/// BCH(18,6) version bits for versions 7..=10 (18 bits, LSB-first).
fn version_bits(version: usize) -> u16 {
    let mut rem = version as u16;
    for _ in 0..12 {
        rem = (rem << 1) ^ (if rem & 0x800 != 0 { VERSION_GEN } else { 0 });
    }
    ((version as u16) << 12) | rem
}

/// Draw both format copies and the always-dark module for `mask`.
fn draw_format_bits(m: &mut Matrix, mask: u8) {
    let bits = format_bits(mask);
    let size = m.size;
    for copy in 0..2 {
        for_each_format_cell(size, copy, |row, col, i| {
            m.set_fn(row, col, bits & (1u16 << i) != 0);
        });
    }
    m.set_fn(size - 8, 8, true);
}

/// Draw both version-information blocks (no-op below version 7).
fn draw_version_bits(m: &mut Matrix, version: usize) {
    let bits = version_bits(version);
    let (size, _) = (m.size, version);
    for_each_version_cell(version, size, |row, col, i| {
        m.set_fn(row, col, (bits as u32) & (1u32 << i) != 0);
    });
}

/// Draw the function patterns; the format bits use a dummy mask that the
/// final `draw_format_bits` call overwrites once the mask is selected.
fn draw_function_patterns(m: &mut Matrix, version: usize) {
    let size = m.size;
    place_geometry(size, version, |row, col, dark| m.set_fn(row, col, dark));
    draw_format_bits(m, 0);
    draw_version_bits(m, version);
}

/// Stream codeword bits through the zigzag module order (upward from the
/// bottom-right column pair, skipping function modules; columns cross the
/// timing pattern between columns 5 and 4). Remainder cells stay light.
fn draw_codewords(m: &mut Matrix, data: &[u8]) {
    let size = m.size;
    let total_bits = data.len() * 8;
    let mut i = 0usize;
    let mut right = size - 1;
    while right >= 1 {
        if right == 6 {
            right = 5;
        }
        let upward = (right + 1) & 2 == 0;
        for vert in 0..size {
            for j in 0..2 {
                let col = right - j;
                let row = if upward { size - 1 - vert } else { vert };
                let idx = row * size + col;
                if !m.func[idx] && i < total_bits {
                    m.dark[idx] = data[i >> 3] & (0x80u8 >> (i & 7)) != 0;
                    i += 1;
                }
            }
        }
        right = right.saturating_sub(2);
    }
    assert_eq!(i, total_bits, "every codeword bit must find a free module");
}

/// True when mask pattern `mask` inverts the module at (`row`, `col`).
fn mask_bit(mask: u8, row: usize, col: usize) -> bool {
    match mask {
        0 => (row + col).is_multiple_of(2),
        1 => row.is_multiple_of(2),
        2 => col.is_multiple_of(3),
        3 => (row + col).is_multiple_of(3),
        4 => (row / 2 + col / 3).is_multiple_of(2),
        5 => (row * col) % 2 + (row * col) % 3 == 0,
        6 => ((row * col) % 2 + (row * col) % 3).is_multiple_of(2),
        _ => ((row + col) % 2 + (row * col) % 3).is_multiple_of(2),
    }
}

/// XOR the mask pattern over every non-function module (involution: applying
/// twice restores the symbol).
fn apply_mask(m: &mut Matrix, mask: u8) {
    let size = m.size;
    for row in 0..size {
        for col in 0..size {
            let idx = row * size + col;
            if !m.func[idx] && mask_bit(mask, row, col) {
                m.dark[idx] = !m.dark[idx];
            }
        }
    }
}

/// Penalty rules 1..=4 (lower is better): runs of five or more, solid 2x2
/// blocks, finder-like 11-module patterns, and deviation from 50% darkness.
fn penalty_score(m: &Matrix) -> usize {
    const PATTERN_A: [bool; 11] = [
        true, false, true, true, true, false, true, false, false, false, true,
    ];
    const PATTERN_B: [bool; 11] = [
        false, false, false, false, true, false, true, true, true, false, true,
    ];

    let size = m.size;
    let mut score = 0usize;

    for horizontal in [true, false] {
        for i in 0..size {
            let at = |j: usize| {
                if horizontal {
                    m.get(i, j)
                } else {
                    m.get(j, i)
                }
            };
            // Rule 1: same-colour runs of five or more.
            let mut run = 1usize;
            for j in 1..size {
                if at(j) == at(j - 1) {
                    run += 1;
                } else {
                    if run >= 5 {
                        score += run - 2;
                    }
                    run = 1;
                }
            }
            if run >= 5 {
                score += run - 2;
            }
            // Rule 3: finder-like 11-module sequences.
            let mut window = [false; 11];
            for j in 0..=size - 11 {
                for (k, slot) in window.iter_mut().enumerate() {
                    *slot = at(j + k);
                }
                if window == PATTERN_A || window == PATTERN_B {
                    score += 40;
                }
            }
        }
    }

    // Rule 2: solid 2x2 blocks.
    for row in 0..size - 1 {
        for col in 0..size - 1 {
            let v = m.get(row, col);
            if m.get(row, col + 1) == v && m.get(row + 1, col) == v && m.get(row + 1, col + 1) == v
            {
                score += 3;
            }
        }
    }

    // Rule 4: darkness ratio in 5% steps away from 50%.
    let total = size * size;
    let dark = m.dark.iter().filter(|&&d| d).count();
    let percent = dark * 100 / total;
    score += 10 * (percent.abs_diff(50) / 5);

    score
}

/// Final assembly: function patterns, data, then best-mask selection.
fn assemble(version: usize, codewords: &[u8]) -> QrCode {
    let mut m = Matrix::new(version);
    draw_function_patterns(&mut m, version);
    draw_codewords(&mut m, codewords);

    // The geometry must leave exactly the codeword stream plus remainder bits.
    let free = m.func.iter().filter(|&&f| !f).count();
    debug_assert_eq!(free, codewords.len() * 8 + REMAINDER_BITS[version - 1]);

    let mut best_score = usize::MAX;
    let mut best_mask = 0u8;
    for mask in 0..8 {
        apply_mask(&mut m, mask);
        draw_format_bits(&mut m, mask);
        let score = penalty_score(&m);
        if score < best_score {
            best_score = score;
            best_mask = mask;
        }
        apply_mask(&mut m, mask); // undo and try the next pattern
    }
    apply_mask(&mut m, best_mask);
    draw_format_bits(&mut m, best_mask);

    QrCode {
        version: version as u8,
        mask: best_mask,
        size: m.size,
        modules: m.dark,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Result of the test-only decoder below.
    #[derive(Debug)]
    struct Decoded {
        version: u8,
        mask: u8,
        text: String,
    }

    /// Rebuild the function-module mask from the specification geometry so
    /// the decoder can skip timing/finder/alignment/format/version cells.
    fn rebuild_function_mask(size: usize, version: usize) -> Vec<bool> {
        let mut func = vec![false; size * size];
        place_geometry(size, version, |row, col, _| func[row * size + col] = true);
        for copy in 0..2 {
            for_each_format_cell(size, copy, |row, col, _| func[row * size + col] = true);
        }
        func[(size - 8) * size + 8] = true;
        for_each_version_cell(version, size, |row, col, _| func[row * size + col] = true);
        func
    }

    /// Decode an encoded symbol back to text, verifying every error-detection
    /// layer along the way: format BCH (both copies), version BCH, mask
    /// round-trip, codeword placement count, per-block Reed–Solomon
    /// remainders, and the byte-mode segment header.
    fn decode(code: &QrCode) -> Decoded {
        let size = code.size;
        let version = (size - 17) / 4;
        assert_eq!(4 * version + 17, size, "size must encode the version");
        let func = rebuild_function_mask(size, version);

        // Format info: read copy 0, verify the BCH remainder, then require
        // copy 1 to be identical.
        let read_format = |copy: u8| -> u16 {
            let mut raw = 0u16;
            for_each_format_cell(size, copy, |row, col, i| {
                if code.module(row, col) {
                    raw |= 1 << i;
                }
            });
            raw
        };
        let raw0 = read_format(0);
        let raw1 = read_format(1);
        assert_eq!(raw0, raw1, "format copies must agree");
        let unmasked = raw0 ^ FORMAT_MASK;
        let mut check = unmasked;
        for bit in (10..15).rev() {
            if check & (1 << bit) != 0 {
                check ^= FORMAT_GEN << (bit - 10);
            }
        }
        assert_eq!(check, 0, "format BCH remainder must vanish");
        let data = unmasked >> 10;
        let ecc = data >> 3;
        assert_eq!(ecc, u16::from(ECC_FORMAT_M), "only level M is encoded");
        let mask = (data & 0b111) as u8;
        assert_eq!(mask, code.mask(), "format mask must match the symbol");

        // Version info on versions 7..=10.
        if version >= 7 {
            let mut raw = 0u32;
            for_each_version_cell(version, size, |row, col, i| {
                if code.module(row, col) {
                    raw |= 1u32 << i;
                }
            });
            assert_eq!(
                raw,
                u32::from(version_bits(version)),
                "version info must match"
            );
        }

        // Unmask, then stream the non-function modules in zigzag order.
        let mut mods = code.modules.clone();
        for row in 0..size {
            for col in 0..size {
                let idx = row * size + col;
                if !func[idx] && mask_bit(mask, row, col) {
                    mods[idx] = !mods[idx];
                }
            }
        }
        let mut bits: Vec<bool> = Vec::new();
        let mut right = size - 1;
        while right >= 1 {
            if right == 6 {
                right = 5;
            }
            let upward = (right + 1) & 2 == 0;
            for vert in 0..size {
                for j in 0..2 {
                    let col = right - j;
                    let row = if upward { size - 1 - vert } else { vert };
                    if !func[row * size + col] {
                        bits.push(mods[row * size + col]);
                    }
                }
            }
            right = right.saturating_sub(2);
        }
        let total_cw = TOTAL_CODEWORDS[version - 1];
        assert_eq!(
            bits.len(),
            total_cw * 8 + REMAINDER_BITS[version - 1],
            "free-module count must match codeword + remainder bits"
        );
        let codewords: Vec<u8> = bits[..total_cw * 8]
            .chunks(8)
            .map(|byte| {
                byte.iter()
                    .fold(0u8, |acc, &bit| (acc << 1) | u8::from(bit))
            })
            .collect();

        // De-interleave back into blocks and check every RS remainder.
        let (ec_len, g1_blocks, g1_data, g2_blocks, g2_data) = RS_BLOCKS_M[version - 1];
        let lens: Vec<usize> = (0..g1_blocks)
            .map(|_| g1_data as usize)
            .chain((0..g2_blocks).map(|_| g2_data as usize))
            .collect();
        let mut blocks: Vec<Vec<u8>> = vec![Vec::new(); lens.len()];
        let mut idx = 0usize;
        let max_data = lens.iter().copied().max().unwrap_or(0);
        for i in 0..max_data {
            for (b, &len) in lens.iter().enumerate() {
                if i < len {
                    blocks[b].push(codewords[idx]);
                    idx += 1;
                }
            }
        }
        for _ in 0..ec_len as usize {
            for block in blocks.iter_mut() {
                block.push(codewords[idx]);
                idx += 1;
            }
        }
        assert_eq!(idx, codewords.len(), "every codeword consumed");
        let generator = rs_generator(ec_len as usize);
        let mut payload: Vec<u8> = Vec::new();
        for (b, block) in blocks.iter().enumerate() {
            let (data_part, ec_part) = block.split_at(lens[b]);
            assert_eq!(
                rs_remainder(data_part, &generator),
                ec_part,
                "RS remainder check failed for block {b}"
            );
            payload.extend_from_slice(data_part);
        }

        // Parse the byte-mode segment header.
        let mut bit_idx = 0usize;
        let mut take = |count: usize| -> u32 {
            let mut value = 0u32;
            for _ in 0..count {
                let bit = payload[bit_idx >> 3] & (0x80u8 >> (bit_idx & 7)) != 0;
                value = (value << 1) | u32::from(bit);
                bit_idx += 1;
            }
            value
        };
        assert_eq!(take(4), 0b0100, "byte mode indicator required");
        let count_bits = if version <= 9 { 8 } else { 16 };
        let count = take(count_bits) as usize;
        let mut text = Vec::with_capacity(count);
        for _ in 0..count {
            text.push(take(8) as u8);
        }
        let text = String::from_utf8(text).expect("byte-mode payload is UTF-8");

        Decoded {
            version: version as u8,
            mask,
            text,
        }
    }

    fn format_string(mask: u8) -> String {
        let bits = format_bits(mask);
        (0..15)
            .rev()
            .map(|i| if bits & (1 << i) != 0 { '1' } else { '0' })
            .collect()
    }

    #[test]
    fn test_format_info_matches_spec() {
        // Golden level-M format strings (ecc bits 00, mask 0..7).
        let golden = [
            "101010000010010",
            "101000100100101",
            "101111001111100",
            "101101101001011",
            "100010111111001",
            "100000011001110",
            "100111110010111",
            "100101010100000",
        ];
        for (mask, expected) in golden.iter().enumerate() {
            assert_eq!(&format_string(mask as u8), expected, "mask {mask}");
        }
    }

    #[test]
    fn test_version_info_matches_spec() {
        let seven: String = (0..18)
            .rev()
            .map(|i| {
                if u32::from(version_bits(7)) & (1 << i) != 0 {
                    '1'
                } else {
                    '0'
                }
            })
            .collect();
        assert_eq!(seven, "000111110010010100");
        let ten: String = (0..18)
            .rev()
            .map(|i| {
                if u32::from(version_bits(10)) & (1 << i) != 0 {
                    '1'
                } else {
                    '0'
                }
            })
            .collect();
        assert_eq!(ten, "001010010011010011");
    }

    #[test]
    fn test_block_tables_are_consistent() {
        for version in 1..=10 {
            let (ec_len, g1b, g1d, g2b, g2d) = RS_BLOCKS_M[version - 1];
            let data = data_codewords(version);
            assert_eq!(
                data,
                g1b as usize * g1d as usize + g2b as usize * g2d as usize
            );
            let ec = ec_len as usize * (g1b as usize + g2b as usize);
            assert_eq!(
                data + ec,
                TOTAL_CODEWORDS[version - 1],
                "version {version} total codewords"
            );
        }
    }

    #[test]
    fn test_capacity_table_and_boundaries() {
        // Spec level-M byte capacities for versions 1..=10.
        let caps = [14, 26, 42, 62, 84, 106, 122, 152, 180, 213];
        for (i, &cap) in caps.iter().enumerate() {
            assert_eq!(max_byte_payload(i + 1), cap, "version {}", i + 1);
        }
        // Exactly 213 bytes fits version 10; 214 overflows every version.
        let ok = QrCode::encode(&"y".repeat(213)).expect("213 bytes fit");
        assert_eq!(ok.version(), 10);
        let err = QrCode::encode(&"y".repeat(214)).expect_err("214 too long");
        assert_eq!(err, QrError::PayloadTooLong { len: 214, max: 213 });
        assert_eq!(
            err.to_string(),
            "QR payload of 214 bytes exceeds level-M version-10 capacity of 213 bytes"
        );
    }

    #[test]
    fn test_data_cell_count_matches_spec() {
        // Free modules must equal total-codeword bits plus remainder bits —
        // this pins the finder/separator/timing/format/alignment geometry.
        for version in 1..=10 {
            let size = version * 4 + 17;
            let func = rebuild_function_mask(size, version);
            let free = func.iter().filter(|&&f| !f).count();
            assert_eq!(
                free,
                TOTAL_CODEWORDS[version - 1] * 8 + REMAINDER_BITS[version - 1],
                "version {version} free-module count"
            );
        }
    }

    #[test]
    fn test_structure_of_small_symbol() {
        let code = QrCode::encode("PlainWeb").expect("encode");
        assert_eq!(code.version(), 1);
        assert_eq!(code.size(), 21);

        // Finder borders and separator.
        for i in 0..7 {
            assert!(code.module(0, i) && code.module(6, i));
            assert!(code.module(i, 0) && code.module(i, 6));
        }
        assert!(!code.module(7, 0) && !code.module(0, 7) && !code.module(7, 7));
        // Finder interior: dark ring at chebyshev 1, light ring at 2.
        assert!(code.module(3, 3) && code.module(2, 3));
        assert!(!code.module(1, 1) && code.module(0, 0));

        // Timing pattern between the finders.
        for i in 8..13 {
            assert_eq!(code.module(6, i), i % 2 == 0, "row 6 timing at {i}");
            assert_eq!(code.module(i, 6), i % 2 == 0, "col 6 timing at {i}");
        }

        // The always-dark module below the second format copy.
        assert!(code.module(code.size() - 8, 8));
        // A masked symbol is neither all-light nor all-dark.
        let dark = code.modules.iter().filter(|&&d| d).count();
        assert!(dark > 0 && dark < code.modules.len());
    }

    #[test]
    fn test_sizes_follow_version_formula() {
        let cases = [(5, 1u8), (70, 5), (110, 7), (130, 8), (170, 9), (213, 10)];
        for (len, expected) in cases {
            let code = QrCode::encode(&"a".repeat(len)).expect("encode");
            assert_eq!(code.version(), expected, "payload len {len}");
            assert_eq!(code.size(), 4 * expected as usize + 17);
        }
    }

    #[test]
    fn test_roundtrip_v1_small_payload() {
        let code = QrCode::encode("hello plainweb").expect("encode");
        assert_eq!(code.version(), 1);
        assert!(code.mask() < 8);
        let decoded = decode(&code);
        assert_eq!(decoded.version, code.version());
        assert_eq!(decoded.mask, code.mask());
        assert_eq!(decoded.text, "hello plainweb");
    }

    #[test]
    fn test_roundtrip_across_versions() {
        // Payload lengths chosen to land on multi-block versions (5, 7) and
        // the two-group versions 8/9 plus the 16-bit-count version 10.
        for (len, expected) in [(70usize, 5u8), (110, 7), (130, 8), (170, 9), (213, 10)] {
            let payload: String = "PlainWeb-QR-".repeat(64)[..len].to_string();
            let code = QrCode::encode(&payload).expect("encode");
            assert_eq!(code.version(), expected, "payload len {len}");
            let decoded = decode(&code);
            assert_eq!(decoded.text, payload, "version {expected}");
        }
    }

    #[test]
    fn test_roundtrip_unicode_payload() {
        let payload = "héllo wörld — plain://nodes.plain?connect=☕";
        let code = QrCode::encode(payload).expect("encode");
        let decoded = decode(&code);
        assert_eq!(decoded.text, payload);
    }

    #[test]
    fn test_roundtrip_connection_link() {
        let addr: SocketAddr = "192.168.1.42:8080".parse().expect("addr");
        let key = PublicKey([0x42u8; 32]);
        let code = QrCode::encode_connection_link(addr, key).expect("encode");
        let decoded = decode(&code);
        assert_eq!(
            system::parse_connection_link(&decoded.text),
            Some((addr, key))
        );
    }

    #[test]
    fn test_roundtrip_ipv6_connection_link() {
        let addr: SocketAddr = "[fd00::1]:7922".parse().expect("addr");
        let key = PublicKey([0x11u8; 32]);
        let link = system::build_connection_link(addr, key);
        let code = QrCode::encode(&link).expect("encode");
        let decoded = decode(&code);
        assert_eq!(decoded.text, link);
        assert_eq!(parse_pairing_payload(&decoded.text), Some((addr, key)));
    }

    #[test]
    fn test_encode_is_deterministic() {
        let a = QrCode::encode("determinism check").expect("encode");
        let b = QrCode::encode("determinism check").expect("encode");
        assert_eq!(a, b);
    }

    #[test]
    fn test_bitmap_and_renderers() {
        let code = QrCode::encode("PlainWeb").expect("encode");
        let width = code.size() + QUIET_ZONE * 2;
        let bitmap = code.bitmap();
        assert_eq!(bitmap.len(), width);
        assert!(bitmap.iter().all(|row| row.len() == width));
        // Quiet zone stays empty.
        assert!(bitmap[0].iter().all(|&dark| !dark));
        assert!(bitmap[width - 1].iter().all(|&dark| !dark));

        let ascii = code.to_ascii();
        assert_eq!(ascii.lines().count(), width);
        assert!(ascii.lines().all(|line| line.chars().count() == width * 2));
        assert!(ascii.contains('█'));

        let svg = code.to_svg();
        assert!(svg.starts_with("<svg"));
        assert!(svg.ends_with("</svg>"));
        assert!(svg.contains(&format!("viewBox=\"0 0 {width} {width}\"")));
        assert!(svg.contains("<path fill=\"#000\""));
        assert!(
            svg.contains("M4 4h1v1h-1z"),
            "first dark module in quiet zone"
        );
    }

    #[test]
    fn test_parse_pairing_payload_canonical_and_compact() {
        let addr: SocketAddr = "10.0.0.9:8080".parse().expect("addr");
        let key = PublicKey([0xABu8; 32]);

        let link = system::build_connection_link(addr, key);
        assert_eq!(parse_pairing_payload(&link), Some((addr, key)));
        assert_eq!(
            parse_pairing_payload(&link),
            system::parse_connection_link(&link)
        );

        let compact = format!("{addr}/{}", key.to_hex());
        assert_eq!(parse_pairing_payload(&compact), Some((addr, key)));
        // Whitespace around a scanned string is tolerated.
        assert_eq!(
            parse_pairing_payload(&format!("  {compact}\n")),
            Some((addr, key))
        );
    }

    #[test]
    fn test_parse_pairing_payload_rejects_garbage() {
        assert_eq!(parse_pairing_payload(""), None);
        assert_eq!(parse_pairing_payload("garbage"), None);
        assert_eq!(parse_pairing_payload("plain://search.plain?q=hi"), None);
        assert_eq!(parse_pairing_payload("10.0.0.1:80/zz"), None); // non-hex
        assert_eq!(parse_pairing_payload("10.0.0.1:80/"), None); // empty key
        assert_eq!(
            parse_pairing_payload(&format!("10.0.0.1:80/{}", "ab".repeat(31))),
            None // 62 hex chars, not 32 bytes
        );
        assert_eq!(
            parse_pairing_payload(&format!("999.999.999.999:80/{}", "ab".repeat(32))),
            None // invalid address
        );
        assert_eq!(parse_pairing_payload("no-port/abcd"), None);
    }
}
