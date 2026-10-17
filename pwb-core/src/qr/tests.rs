use super::constants::{
    ECC_FORMAT_M, FORMAT_GEN, FORMAT_MASK, REMAINDER_BITS, RS_BLOCKS_M, TOTAL_CODEWORDS,
};
use super::gf::{data_codewords, max_byte_payload, rs_generator, rs_remainder};
use super::matrix::{
    for_each_format_cell, for_each_version_cell, format_bits, mask_bit, place_geometry,
    version_bits,
};
use super::*;
use crate::crypto::PublicKey;
use crate::system;
use std::net::SocketAddr;

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
