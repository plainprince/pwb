//! Symbol grid: geometry, function patterns, masking, and assembly.

use super::code::QrCode;
use super::constants::{
    ALIGNMENT_CENTERS, ECC_FORMAT_M, FORMAT_GEN, FORMAT_MASK, REMAINDER_BITS, VERSION_GEN,
};

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
pub(super) fn place_geometry(size: usize, version: usize, mut set: impl FnMut(usize, usize, bool)) {
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
pub(super) fn for_each_format_cell(size: usize, copy: u8, mut f: impl FnMut(usize, usize, usize)) {
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
pub(super) fn for_each_version_cell(
    version: usize,
    size: usize,
    mut f: impl FnMut(usize, usize, usize),
) {
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
pub(super) fn format_bits(mask: u8) -> u16 {
    let data = u16::from(ECC_FORMAT_M) << 3 | u16::from(mask);
    let mut rem = data;
    for _ in 0..10 {
        rem = (rem << 1) ^ (if rem & 0x200 != 0 { FORMAT_GEN } else { 0 });
    }
    ((data << 10) | rem) ^ FORMAT_MASK
}

/// BCH(18,6) version bits for versions 7..=10 (18 bits, LSB-first).
pub(super) fn version_bits(version: usize) -> u16 {
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
pub(super) fn mask_bit(mask: u8, row: usize, col: usize) -> bool {
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
pub(super) fn assemble(version: usize, codewords: &[u8]) -> QrCode {
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
