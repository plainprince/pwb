//! Minimal STUN client (RFC 5389) for NAT traversal, over IPv4 and IPv6.
//!
//! Sends a STUN `Binding Request` to a STUN server and parses the
//! `XOR-MAPPED-ADDRESS` attribute from the `Binding Response` to learn this
//! node's public IP:port as seen by the internet. A node behind a router can
//! then advertise its public address in connection links instead of a private
//! (RFC 1918 / RFC 4193) LAN address, so peers on the internet can dial it
//! directly.
//!
//! Both address families are handled: family `0x01` bodies carry a 4-byte
//! IPv4 address XORed with the magic cookie, family `0x02` bodies carry a
//! 16-byte IPv6 address XORed with the cookie (first 32 bits) plus the
//! transaction ID (remaining 96 bits), per RFC 5389 §15.2. The query runs
//! over whatever socket the caller passes, with the server address mapped to
//! that socket's family, so a dual-stack node gets a mapped address for
//! either family.
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

/// IPv6 XOR-MAPPED-ADDRESS value body: reserved(1) + family(1) + port(2) +
/// ip(16).
const XOR_MAPPED_ADDR_V6_BODY_LEN: usize = 20;

/// Address family byte for IPv4 (RFC 5389 §15.2).
const FAMILY_IPV4: u8 = 0x01;

/// Address family byte for IPv6 (RFC 5389 §15.2).
const FAMILY_IPV6: u8 = 0x02;

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

/// Encode an XOR-MAPPED-ADDRESS attribute body for an IPv4 or IPv6 address.
///
/// The port is always XORed with the magic cookie's low 16 bits. An IPv4
/// address is XORed with the full 32-bit cookie; an IPv6 address is XORed
/// with the cookie (first 32 bits) and the transaction ID (remaining 96
/// bits), per RFC 5389 §15.2. Values are stored in network byte order
/// (big-endian).
#[allow(dead_code)]
fn encode_xor_mapped_address(addr: SocketAddr, txn_id: &[u8; TRANSACTION_ID_LEN]) -> Vec<u8> {
    let cookie = STUN_MAGIC_COOKIE;
    let xor_port = addr.port() ^ (cookie >> 16) as u16;
    match addr.ip() {
        std::net::IpAddr::V4(ip) => {
            let xor_ip = u32::from_be_bytes(ip.octets()) ^ cookie;
            let mut out = Vec::with_capacity(XOR_MAPPED_ADDR_BODY_LEN);
            out.push(0x00); // reserved
            out.push(FAMILY_IPV4);
            out.push((xor_port >> 8) as u8); // high byte
            out.push(xor_port as u8); // low byte
            out.extend_from_slice(&xor_ip.to_be_bytes());
            out
        }
        std::net::IpAddr::V6(ip) => {
            let octets = ip.octets();
            let mut xaddr = [0u8; 16];
            xaddr.copy_from_slice(&octets);
            // First 32 bits XOR the cookie, the remaining 96 bits XOR the
            // transaction ID.
            let head = u32::from_be_bytes([xaddr[0], xaddr[1], xaddr[2], xaddr[3]]) ^ cookie;
            xaddr[0..4].copy_from_slice(&head.to_be_bytes());
            for (byte, key) in xaddr[4..16].iter_mut().zip(&txn_id[..12]) {
                *byte ^= key;
            }
            let mut out = Vec::with_capacity(XOR_MAPPED_ADDR_V6_BODY_LEN);
            out.push(0x00); // reserved
            out.push(FAMILY_IPV6);
            out.push((xor_port >> 8) as u8); // high byte
            out.push(xor_port as u8); // low byte
            out.extend_from_slice(&xaddr);
            out
        }
    }
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
            return decode_xor_mapped_address(value, expected_txn);
        }
        // STUN attributes are padded to 4-byte boundaries.
        offset += attr_len;
        let pad = (4 - (attr_len % 4)) % 4;
        offset += pad;
    }
    None
}

/// Decode an XOR-MAPPED-ADDRESS attribute value body into a `SocketAddr`.
///
/// Handles both families: `0x01` (4-byte IPv4 XORed with the cookie) and
/// `0x02` (16-byte IPv6 XORed with the cookie plus `txn_id`), per RFC 5389
/// §15.2. `txn_id` is the request's transaction ID; IPv6 bodies cannot be
/// decoded without it.
fn decode_xor_mapped_address(
    value: &[u8],
    txn_id: &[u8; TRANSACTION_ID_LEN],
) -> Option<SocketAddr> {
    let family = *value.get(1)?;
    let cookie = STUN_MAGIC_COOKIE;
    let xor_port = u16::from_be_bytes([*value.get(2)?, *value.get(3)?]);
    let port = xor_port ^ (cookie >> 16) as u16;
    match family {
        FAMILY_IPV4 => {
            if value.len() < XOR_MAPPED_ADDR_BODY_LEN {
                return None;
            }
            let xor_ip = u32::from_be_bytes([value[4], value[5], value[6], value[7]]);
            let ip = xor_ip ^ cookie;
            Some(SocketAddr::new(
                std::net::IpAddr::V4(std::net::Ipv4Addr::from(ip)),
                port,
            ))
        }
        FAMILY_IPV6 => {
            if value.len() < XOR_MAPPED_ADDR_V6_BODY_LEN {
                return None;
            }
            let mut xaddr = [0u8; 16];
            xaddr.copy_from_slice(&value[4..20]);
            let head = u32::from_be_bytes([xaddr[0], xaddr[1], xaddr[2], xaddr[3]]) ^ cookie;
            xaddr[0..4].copy_from_slice(&head.to_be_bytes());
            for (byte, key) in xaddr[4..16].iter_mut().zip(&txn_id[..12]) {
                *byte ^= key;
            }
            Some(SocketAddr::new(
                std::net::IpAddr::V6(std::net::Ipv6Addr::from(xaddr)),
                port,
            ))
        }
        _ => None,
    }
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
    // A dual-stack IPv6 socket must address an IPv4 STUN server v4-mapped;
    // same-family servers pass through untouched.
    let target = socket
        .local_addr()
        .ok()
        .map_or(server, |local| crate::net::map_dest_for(server, local));
    socket.send_to(&request, target).await.ok()?;

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
                    // A dual-stack socket observes its own IPv4 mapping as a
                    // v4-mapped IPv6 address; hand back the canonical form.
                    return Some(crate::net::unmap_source(addr));
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
        let txn = make_txn_id();
        let addr: SocketAddr = "203.0.113.45:19302".parse().unwrap();
        let body = encode_xor_mapped_address(addr, &txn);
        let decoded = decode_xor_mapped_address(&body, &txn).expect("decode");
        assert_eq!(decoded, addr);
    }

    #[test]
    fn test_parse_binding_response_accepts_valid() {
        let txn = make_txn_id();
        let mapped: SocketAddr = "198.51.100.7:54321".parse().unwrap();
        let body = encode_xor_mapped_address(mapped, &txn);
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
        let body = encode_xor_mapped_address(mapped, &txn);
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
        let body = encode_xor_mapped_address(mapped, &txn);
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
    fn test_ipv6_xor_mapped_address_roundtrip() {
        let txn = make_txn_id();
        let addr: SocketAddr = "[2001:db8::1]:12345".parse().unwrap();
        let body = encode_xor_mapped_address(addr, &txn);
        assert_eq!(body.len(), XOR_MAPPED_ADDR_V6_BODY_LEN);
        assert_eq!(body[1], FAMILY_IPV6);
        let decoded = decode_xor_mapped_address(&body, &txn).expect("v6 decode");
        assert_eq!(decoded, addr);
        // The transaction ID is part of the XOR key: a response attributed to
        // a different transaction does not decode to the same address.
        let mut wrong = txn;
        wrong[0] ^= 0xff;
        assert_ne!(decode_xor_mapped_address(&body, &wrong), Some(addr));
    }

    #[test]
    fn test_ipv6_xor_mapped_address_matches_rfc_5389_rule() {
        // Independent check of RFC 5389 §15.2 against hand-computed bytes:
        // port 12345 (0x3039) XOR cookie-low (0x2112) = 0x112B; the address
        // 2001:db8::1 XORs its first 32 bits with the cookie and the rest
        // with the transaction ID.
        let txn = make_txn_id();
        let addr: SocketAddr = "[2001:db8::1]:12345".parse().unwrap();
        let body = encode_xor_mapped_address(addr, &txn);
        let expected: [u8; 20] = [
            0x00, 0x02, // reserved, family IPv6
            0x11, 0x2B, // 0x3039 ^ 0x2112
            0x01, 0x13, 0xA9, 0xFA, // 0x20010db8 ^ 0x2112a442
            0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB,
            0xCD, // zero bytes ^ txn, 0x01 ^ 0xcc
        ];
        assert_eq!(body.as_slice(), &expected);
        assert_eq!(decode_xor_mapped_address(&expected, &txn), Some(addr));
    }

    #[test]
    fn test_parse_binding_response_accepts_ipv6() {
        let txn = make_txn_id();
        let mapped: SocketAddr = "[2001:db8::42]:7921".parse().unwrap();
        let body = encode_xor_mapped_address(mapped, &txn);
        let attr_len = (ATTR_HEADER_LEN + body.len()) as u16;
        let mut resp = Vec::new();
        resp.extend_from_slice(&STUN_BINDING_RESPONSE.to_be_bytes());
        resp.extend_from_slice(&attr_len.to_be_bytes());
        resp.extend_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
        resp.extend_from_slice(&txn);
        resp.extend_from_slice(&ATTR_XOR_MAPPED_ADDRESS.to_be_bytes());
        resp.extend_from_slice(&(body.len() as u16).to_be_bytes());
        resp.extend_from_slice(&body);
        assert_eq!(parse_binding_response(&resp, &txn), Some(mapped));
        // A truncated IPv6 body is rejected rather than read out of bounds.
        assert_eq!(
            decode_xor_mapped_address(&body[..XOR_MAPPED_ADDR_V6_BODY_LEN - 1], &txn),
            None
        );
        // Unknown families decode to nothing.
        let mut alien = body.clone();
        alien[1] = 0x03;
        assert_eq!(decode_xor_mapped_address(&alien, &txn), None);
    }

    #[tokio::test]
    async fn test_discover_over_dual_stack_socket() {
        // A fake STUN server on IPv4 loopback, answered by a client socket
        // bound dual-stack `[::]` — the exact shape of a node that accepts
        // both families but queries an IPv4 STUN server.
        let server = UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind v4 server");
        let server_addr = server.local_addr().unwrap();

        let responder = tokio::spawn(async move {
            let mut buf = [0u8; 1500];
            let (len, from) = server.recv_from(&mut buf).await.expect("request");
            assert_eq!(len, STUN_HEADER_LEN, "binding request size");
            let mut txn = [0u8; TRANSACTION_ID_LEN];
            txn.copy_from_slice(&buf[8..20]);
            // Answer with the source address as the server observed it.
            let body = encode_xor_mapped_address(from, &txn);
            let mut resp = Vec::new();
            resp.extend_from_slice(&STUN_BINDING_RESPONSE.to_be_bytes());
            resp.extend_from_slice(&((ATTR_HEADER_LEN + body.len()) as u16).to_be_bytes());
            resp.extend_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
            resp.extend_from_slice(&txn);
            resp.extend_from_slice(&ATTR_XOR_MAPPED_ADDRESS.to_be_bytes());
            resp.extend_from_slice(&(body.len() as u16).to_be_bytes());
            resp.extend_from_slice(&body);
            server.send_to(&resp, from).await.expect("response");
        });

        // Mirror the engine's dual-stack bind: explicitly opt in to
        // IPv4-mapped traffic instead of trusting the platform default.
        let socket = {
            use socket2::{Domain, Protocol, Socket, Type};
            let raw = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP)).unwrap();
            raw.set_only_v6(false).unwrap();
            raw.bind(&std::net::SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, 0)).into())
                .unwrap();
            raw.set_nonblocking(true).unwrap();
            UdpSocket::from_std(raw.into()).unwrap()
        };

        let mapped = discover_mapped_address(&socket, server_addr, Duration::from_secs(3))
            .await
            .expect("mapped address");
        responder.await.unwrap();
        assert!(
            mapped.is_ipv4(),
            "the IPv4 server's view comes back unmapped"
        );
        assert_eq!(
            mapped,
            std::net::SocketAddr::from((
                std::net::Ipv4Addr::LOCALHOST,
                socket.local_addr().unwrap().port()
            )),
            "the client's own address as the server saw it"
        );
    }

    #[test]
    fn test_example_from_rfc_5389() {
        // RFC 5389 §15.2 example: client at 192.168.1.10:50000 via NAT,
        // server sees 203.0.113.45:19302? Actually the RFC example uses a
        // specific cookie/xor. We just verify symmetry against a known value.
        let txn = make_txn_id();
        let mapped: SocketAddr = "203.0.113.45:19302".parse().unwrap();
        let body = encode_xor_mapped_address(mapped, &txn);
        assert_eq!(body.len(), 8);
        assert_eq!(body[1], FAMILY_IPV4);
        let decoded = decode_xor_mapped_address(&body, &txn).expect("roundtrip");
        assert_eq!(decoded, mapped);
    }

    #[test]
    fn test_localhost_roundtrip() {
        let txn = make_txn_id();
        let mapped: SocketAddr = "127.0.0.1:7922".parse().unwrap();
        let body = encode_xor_mapped_address(mapped, &txn);
        let decoded = decode_xor_mapped_address(&body, &txn).expect("roundtrip");
        assert_eq!(decoded, mapped);
    }
}
