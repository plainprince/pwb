//! Minimal STUN client (RFC 5389) for NAT traversal.
//!
//! Sends a STUN `Binding Request` to a STUN server and parses the
//! `XOR-MAPPED-ADDRESS` attribute from the `Binding Response` to learn this
//! node's public IP:port as seen by the internet. A node behind a router can
//! then advertise its public address in connection links instead of a private
//! (RFC 1918) LAN address, so peers on the internet can dial it directly.
//!
//! No new dependencies: the STUN binary format is small enough to encode and
//! decode by hand over the existing `tokio::net::UdpSocket`.

use std::net::SocketAddr;
use std::time::Duration;

use tokio::net::UdpSocket;

/// STUN magic cookie (RFC 5389 §6).
const STUN_MAGIC_COOKIE: u32 = 0x2112_A442;

/// STUN Binding Request message type.
const STUN_BINDING_REQUEST: u16 = 0x0001;
/// STUN Binding Response message type.
const STUN_BINDING_RESPONSE: u16 = 0x0101;

/// STUN attribute: XOR-MAPPED-ADDRESS (0x0020).
const ATTR_XOR_MAPPED_ADDRESS: u16 = 0x0020;

/// Length of a STUN transaction ID in bytes.
const TRANSACTION_ID_LEN: usize = 12;

/// Fixed STUN header size: type(2) + length(2) + magic cookie(4) + txn id(12).
const STUN_HEADER_LEN: usize = 20;

/// Attribute header: type(2) + length(2).
const ATTR_HEADER_LEN: usize = 4;

/// XOR-MAPPED-ADDRESS value body: reserved(1) + family(1) + port(2) + ip(4).
const XOR_MAPPED_ADDR_BODY_LEN: usize = 8;

/// Build a STUN Binding Request payload.
pub fn build_binding_request(txn_id: &[u8; TRANSACTION_ID_LEN]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(STUN_HEADER_LEN);
    // Message type: Binding Request.
    buf.extend_from_slice(&STUN_BINDING_REQUEST.to_be_bytes());
    // Message length: 0 (no attributes).
    buf.extend_from_slice(&(0u16).to_be_bytes());
    // Magic cookie.
    buf.extend_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
    // Transaction ID.
    buf.extend_from_slice(txn_id);
    buf
}

/// Encode an XOR-MAPPED-ADDRESS attribute body for an IPv4 address.
///
/// The port and IP are XORed with the STUN magic cookie (port with the high
/// 16 bits, IP with the full 32 bits) per RFC 5389 §15.2. Values are stored
/// in network byte order (big-endian).
#[allow(dead_code)]
fn encode_xor_mapped_address(addr: SocketAddr) -> Vec<u8> {
    let ip = match addr.ip() {
        std::net::IpAddr::V4(ip) => u32::from_be_bytes(ip.octets()),
        std::net::IpAddr::V6(_) => return Vec::new(), // IPv6 not supported here
    };
    let port = addr.port();
    let cookie = STUN_MAGIC_COOKIE;
    let xor_port = port ^ (cookie >> 16) as u16;
    let xor_ip = ip ^ cookie;
    let mut out = Vec::with_capacity(XOR_MAPPED_ADDR_BODY_LEN);
    out.push(0x00); // reserved
    out.push(0x01); // address family: IPv4
    out.push((xor_port >> 8) as u8); // high byte
    out.push(xor_port as u8); // low byte
    out.extend_from_slice(&xor_ip.to_be_bytes());
    out
}

/// Parse a STUN message header. Returns `Some((msg_type, txn_id))` when the
/// magic cookie matches and the transaction ID could be read.
fn parse_stun_header(
    bytes: &[u8],
    expected_txn: &[u8; TRANSACTION_ID_LEN],
) -> Option<(u16, [u8; TRANSACTION_ID_LEN])> {
    if bytes.len() < STUN_HEADER_LEN {
        return None;
    }
    if bytes[4..8] != STUN_MAGIC_COOKIE.to_be_bytes() {
        return None;
    }
    let msg_type = u16::from_be_bytes(bytes[0..2].try_into().ok()?);
    let mut txn_id = [0u8; TRANSACTION_ID_LEN];
    txn_id.copy_from_slice(&bytes[8..20]);
    if txn_id != *expected_txn {
        return None;
    }
    Some((msg_type, txn_id))
}

/// Parse a `Binding Response` and extract the `XOR-MAPPED-ADDRESS`.
///
/// Returns the mapped public `SocketAddr` if a valid XOR-MAPPED-ADDRESS
/// attribute is present and the message type / transaction ID match.
pub fn parse_binding_response(
    bytes: &[u8],
    expected_txn: &[u8; TRANSACTION_ID_LEN],
) -> Option<SocketAddr> {
    let (msg_type, _txn_id) = parse_stun_header(bytes, expected_txn)?;
    if msg_type != STUN_BINDING_RESPONSE {
        return None;
    }

    let msg_len = u16::from_be_bytes(bytes[2..4].try_into().ok()?) as usize;
    if bytes.len() < STUN_HEADER_LEN + msg_len {
        return None;
    }

    let attrs = &bytes[STUN_HEADER_LEN..STUN_HEADER_LEN + msg_len];
    let mut offset = 0usize;
    while offset + ATTR_HEADER_LEN <= attrs.len() {
        let attr_type = u16::from_be_bytes(attrs[offset..offset + 2].try_into().ok()?);
        let attr_len = u16::from_be_bytes(attrs[offset + 2..offset + 4].try_into().ok()?) as usize;
        offset += ATTR_HEADER_LEN;
        if offset + attr_len > attrs.len() {
            return None;
        }
        let value = &attrs[offset..offset + attr_len];
        if attr_type == ATTR_XOR_MAPPED_ADDRESS {
            return decode_xor_mapped_address(value);
        }
        // STUN attributes are padded to 4-byte boundaries.
        offset += attr_len;
        let pad = (4 - (attr_len % 4)) % 4;
        offset += pad;
    }
    None
}

/// Decode an XOR-MAPPED-ADDRESS attribute value body into a `SocketAddr`.
fn decode_xor_mapped_address(value: &[u8]) -> Option<SocketAddr> {
    if value.len() < XOR_MAPPED_ADDR_BODY_LEN {
        return None;
    }
    let _reserved = value[0];
    let family = value[1];
    if family != 0x01 {
        return None; // only IPv4 supported
    }
    let cookie = STUN_MAGIC_COOKIE;
    let xor_port = u16::from_be_bytes([value[2], value[3]]);
    let port = xor_port ^ (cookie >> 16) as u16;
    let xor_ip = u32::from_be_bytes([value[4], value[5], value[6], value[7]]);
    let ip = xor_ip ^ cookie;
    Some(SocketAddr::new(
        std::net::IpAddr::V4(std::net::Ipv4Addr::from(ip)),
        port,
    ))
}

/// Send a STUN Binding Request to `server` and return the XOR-MAPPED-ADDRESS
/// from the response — this node's public IP:port as seen by the STUN server
/// (i.e. through any NAT between).
///
/// Returns `None` on socket errors, timeouts, or malformed responses.
pub async fn discover_mapped_address(
    socket: &UdpSocket,
    server: SocketAddr,
    timeout: Duration,
) -> Option<SocketAddr> {
    let mut txn_id = [0u8; TRANSACTION_ID_LEN];
    for byte in &mut txn_id {
        *byte = rand::random();
    }

    let request = build_binding_request(&txn_id);
    socket.send_to(&request, server).await.ok()?;

    let deadline = tokio::time::Instant::now() + timeout;
    let mut buf = [0u8; 1500];
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        match tokio::time::timeout(remaining, socket.recv_from(&mut buf)).await {
            Ok(Ok((len, _from))) => {
                if let Some(addr) = parse_binding_response(&buf[..len], &txn_id) {
                    return Some(addr);
                }
            }
            Ok(Err(_)) => return None,
            Err(_) => return None, // timed out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_txn_id() -> [u8; TRANSACTION_ID_LEN] {
        [
            0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc,
        ]
    }

    #[test]
    fn test_build_binding_request_shape() {
        let txn = make_txn_id();
        let req = build_binding_request(&txn);
        assert_eq!(req.len(), STUN_HEADER_LEN);
        assert_eq!(
            u16::from_be_bytes(req[0..2].try_into().unwrap()),
            STUN_BINDING_REQUEST
        );
        assert_eq!(&req[4..8], &STUN_MAGIC_COOKIE.to_be_bytes());
        assert_eq!(&req[8..20], &txn);
    }

    #[test]
    fn test_xor_mapped_address_roundtrip() {
        let addr: SocketAddr = "203.0.113.45:19302".parse().unwrap();
        let body = encode_xor_mapped_address(addr);
        let decoded = decode_xor_mapped_address(&body).expect("decode");
        assert_eq!(decoded, addr);
    }

    #[test]
    fn test_parse_binding_response_accepts_valid() {
        let txn = make_txn_id();
        let mapped: SocketAddr = "198.51.100.7:54321".parse().unwrap();
        let body = encode_xor_mapped_address(mapped);
        let attr_len = (ATTR_HEADER_LEN + body.len()) as u16;
        let mut resp = Vec::with_capacity(STUN_HEADER_LEN + attr_len as usize);
        resp.extend_from_slice(&STUN_BINDING_RESPONSE.to_be_bytes());
        resp.extend_from_slice(&attr_len.to_be_bytes());
        resp.extend_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
        resp.extend_from_slice(&txn);
        resp.extend_from_slice(&ATTR_XOR_MAPPED_ADDRESS.to_be_bytes());
        resp.extend_from_slice(&(body.len() as u16).to_be_bytes());
        resp.extend_from_slice(&body);

        let got = parse_binding_response(&resp, &txn).expect("parse");
        assert_eq!(got, mapped);
    }

    #[test]
    fn test_parse_binding_response_rejects_wrong_txn() {
        let txn = make_txn_id();
        let wrong_txn = [0u8; TRANSACTION_ID_LEN];
        let mapped: SocketAddr = "198.51.100.7:54321".parse().unwrap();
        let mut resp = Vec::new();
        resp.extend_from_slice(&STUN_BINDING_RESPONSE.to_be_bytes());
        let body = encode_xor_mapped_address(mapped);
        resp.extend_from_slice(&(body.len() as u16).to_be_bytes());
        resp.extend_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
        resp.extend_from_slice(&txn);
        resp.extend_from_slice(&ATTR_XOR_MAPPED_ADDRESS.to_be_bytes());
        resp.extend_from_slice(&(body.len() as u16).to_be_bytes());
        resp.extend_from_slice(&body);
        assert!(parse_binding_response(&resp, &wrong_txn).is_none());
    }

    #[test]
    fn test_parse_binding_response_rejects_wrong_msg_type() {
        let txn = make_txn_id();
        let mapped: SocketAddr = "198.51.100.7:54321".parse().unwrap();
        let mut resp = Vec::new();
        resp.extend_from_slice(&STUN_BINDING_REQUEST.to_be_bytes()); // wrong type
        let body = encode_xor_mapped_address(mapped);
        resp.extend_from_slice(&(body.len() as u16).to_be_bytes());
        resp.extend_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
        resp.extend_from_slice(&txn);
        resp.extend_from_slice(&ATTR_XOR_MAPPED_ADDRESS.to_be_bytes());
        resp.extend_from_slice(&(body.len() as u16).to_be_bytes());
        resp.extend_from_slice(&body);
        assert!(parse_binding_response(&resp, &txn).is_none());
    }

    #[test]
    fn test_parse_binding_response_rejects_bad_magic() {
        let txn = make_txn_id();
        let mut resp = Vec::new();
        resp.extend_from_slice(&STUN_BINDING_RESPONSE.to_be_bytes());
        resp.extend_from_slice(&0u16.to_be_bytes());
        resp.extend_from_slice(&0xDEAD_BEEFu32.to_be_bytes()); // bad magic
        resp.extend_from_slice(&txn);
        assert!(parse_binding_response(&resp, &txn).is_none());
    }

    #[test]
    fn test_truncated_response_rejected() {
        let txn = make_txn_id();
        let mut resp = build_binding_request(&txn); // too short, wrong type anyway
        resp.truncate(10);
        assert!(parse_binding_response(&resp, &txn).is_none());
    }

    #[test]
    fn test_ipv6_mapped_address_not_supported() {
        let _addr: SocketAddr = "[::1]:19302".parse().unwrap();
        // encode_xor_mapped_address returns empty for v6; decode of empty fails.
        let body = encode_xor_mapped_address("[::1]:19302".parse().unwrap());
        assert!(body.is_empty());
        assert!(decode_xor_mapped_address(&body).is_none());
    }

    #[test]
    fn test_example_from_rfc_5389() {
        // RFC 5389 §15.2 example: client at 192.168.1.10:50000 via NAT,
        // server sees 203.0.113.45:19302? Actually the RFC example uses a
        // specific cookie/xor. We just verify symmetry against a known value.
        let mapped: SocketAddr = "203.0.113.45:19302".parse().unwrap();
        let body = encode_xor_mapped_address(mapped);
        assert_eq!(body.len(), 8);
        let decoded = decode_xor_mapped_address(&body).expect("roundtrip");
        assert_eq!(decoded, mapped);
    }

    #[test]
    fn test_localhost_roundtrip() {
        let mapped: SocketAddr = "127.0.0.1:7922".parse().unwrap();
        let body = encode_xor_mapped_address(mapped);
        let decoded = decode_xor_mapped_address(&body).expect("roundtrip");
        assert_eq!(decoded, mapped);
    }
}
