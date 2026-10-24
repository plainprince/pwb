//! Onion-routed message transport for [`super::NodeEngine`].
//!
//! [`OnionCell`] carries only [`crate::onion::PAYLOAD_MAX`] (118) bytes, which
//! is smaller than almost every JSON-encoded [`super::NetworkMessage`]. This
//! module adds the missing multi-cell framing:
//!
//! ```text
//! cell payload = "PON1" | message id (16) | total len (2) | index (2) | count (2) | chunk
//! message      = "PON4" | kind (1) | request id (16) | sender key (32)
//!              | return hops (1 = count, then count x [family (1) | addr | port (2) | dh key (32)])
//!              | payload len (4) | signature (64) | JSON message | random padding
//! ```
//!
//! The originator signs the reassembled message with its identity key, so the
//! exit hop can attribute the request even though the onion path hides the
//! origin address. Every fragment is independently encapsulated and routed;
//! an observer sees only fixed 512-byte cells.
//!
//! ## Fixed-size envelopes
//!
//! Multi-cell messages are padded to a fixed 4 KiB envelope, or to the 48 KiB
//! maximum when they do not fit. `total len` and the number of cells therefore
//! take only one of two values per circuit, so an observer cannot correlate a
//! bulk transfer's size with its cell count. The real message length travels
//! inside the signed envelope; the trailing padding is random and ignored
//! after signature verification.
//!
//! ## Return circuits
//!
//! A request can carry a **return path**: the relays (entry first) and the
//! entry hop's dial address that a responder should use to get an answer back
//! to the originator. The originator builds it from a suffix of the outbound
//! path plus its own onion key appended, so the responder never learns the
//! originator's address — it only sees the first return relay. Replies are
//! correlated by the request id and delivered to
//! [`super::NodeEngine::send_onion_request`], which waits for them.
//!
//! Reassembly is bounded (entry count, buffered bytes, and TTL) so a peer
//! cannot pin memory by sending incomplete fragment sets.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::{Duration, Instant};

use rand::rngs::OsRng;
use rand::RngCore;

use crate::crypto::{DhPublicKey, Keypair, PublicKey, Signature};
use crate::onion::PAYLOAD_MAX;

/// Domain separation for onion-delivered message signatures.
pub(crate) const ONION_SIGN_PREFIX: &[u8] = b"PWBONION1";

const FRAGMENT_MAGIC: [u8; 4] = *b"PON1";
const MESSAGE_MAGIC: [u8; 4] = *b"PON4";
const ID_LEN: usize = 16;
const FRAGMENT_HEADER_LEN: usize = 4 + ID_LEN + 2 + 2 + 2;
const MAX_CHUNK: usize = PAYLOAD_MAX - FRAGMENT_HEADER_LEN;
/// `magic | kind | request id | sender | return count | payload len | signature`.
const INNER_HEADER_LEN: usize = 4 + 1 + ID_LEN + 32 + 1 + 4 + 64;
/// Largest reassembled request accepted from an onion exit.
const MAX_ONION_MESSAGE: usize = 48 * 1024;
/// Every message that fits is padded to this size before fragmenting.
const PADDED_ENVELOPE: usize = 4 * 1024;
/// Largest number of relays a return path may name (the onion path maximum).
const MAX_RETURN_HOPS: usize = crate::onion::MAX_LAYER_SLOTS;
const ASSEMBLY_TTL: Duration = Duration::from_secs(30);
const MAX_ASSEMBLIES: usize = 256;
const MAX_BUFFERED_BYTES: usize = 2 * 1024 * 1024;

/// What an onion-delivered message is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnionKind {
    /// A request expecting an answer on the attached return path.
    Request = 0,
    /// An answer to an earlier request, correlated by request id.
    Reply = 1,
}

impl OnionKind {
    fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(OnionKind::Request),
            1 => Some(OnionKind::Reply),
            _ => None,
        }
    }

    fn to_byte(self) -> u8 {
        self as u8
    }
}

/// Where a responder should send an answer so it reaches the (hidden)
/// originator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnionReturnPath {
    /// Dial address of the first return relay (the request's second-to-last
    /// hop). The responder sends the reply cells here.
    pub first_hop: SocketAddr,
    /// Relays to traverse, entry first, ending at the originator's onion key.
    pub path: Vec<DhPublicKey>,
}

impl OnionReturnPath {
    /// Whether the path satisfies the onion layer's 3..=6 unique-relay rule.
    pub fn is_valid(&self) -> bool {
        crate::onion::validate_path(&self.path).is_ok()
    }
}

/// A request delivered by an onion exit, with the information needed to send
/// an answer back through the same circuit.
#[derive(Debug, Clone, PartialEq)]
pub struct OnionRequest {
    /// The authenticated originator identity (never its address).
    pub sender: PublicKey,
    /// Correlation id to echo on the reply.
    pub request_id: [u8; ID_LEN],
    /// Return path, when the originator asked for an answer.
    pub return_path: Option<OnionReturnPath>,
    /// The decoded request message.
    pub message: super::messages::NetworkMessage,
}

impl OnionRequest {
    /// Whether an answer can be routed back to this requester.
    pub fn can_reply(&self) -> bool {
        self.return_path
            .as_ref()
            .is_some_and(OnionReturnPath::is_valid)
    }
}

/// A reassembled and signature-verified onion message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnionMessage {
    /// Request or reply.
    pub kind: OnionKind,
    /// Correlation id chosen by the originator.
    pub request_id: [u8; ID_LEN],
    /// The originator's identity key (the key that signed the message).
    pub sender: PublicKey,
    /// For requests that want an answer: where to send it.
    pub return_path: Option<OnionReturnPath>,
    /// The decoded [`super::NetworkMessage`] on the wire, still serialized.
    pub message: Vec<u8>,
}

/// Split one signed message into per-cell onion payloads.
///
/// `return_path` is attached only to requests; replies carry it as `None`.
pub(crate) fn encode_message_with(
    keypair: &Keypair,
    kind: OnionKind,
    request_id: [u8; ID_LEN],
    return_path: Option<&OnionReturnPath>,
    message: &[u8],
) -> Option<Vec<Vec<u8>>> {
    // Return hops: family-tagged address plus the relay's X25519 key.
    let mut return_bytes = Vec::new();
    if let (Some(return_path), OnionKind::Request) = (return_path, kind) {
        if return_path.path.is_empty() || return_path.path.len() > MAX_RETURN_HOPS {
            return None;
        }
        return_bytes.push(return_path.path.len() as u8);
        for (index, hop) in return_path.path.iter().enumerate() {
            let addr = if index == 0 {
                return_path.first_hop
            } else {
                // Only the entry hop's address is needed on the wire; the
                // later hops resolve from the relay tables they traverse.
                SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0)
            };
            encode_addr(&addr, &mut return_bytes);
            return_bytes.extend_from_slice(&hop.0);
        }
    } else {
        return_bytes.push(0);
    }

    if message.len() > u32::MAX as usize {
        return None;
    }
    let payload_len = message.len() as u32;
    let mut inner = Vec::with_capacity(INNER_HEADER_LEN + return_bytes.len() + message.len());
    inner.extend_from_slice(&MESSAGE_MAGIC);
    inner.push(kind.to_byte());
    inner.extend_from_slice(&request_id);
    inner.extend_from_slice(&keypair.public.0);
    inner.extend_from_slice(&return_bytes);
    inner.extend_from_slice(&payload_len.to_le_bytes());
    let signature_offset = inner.len();
    inner.extend_from_slice(&[0u8; 64]);
    inner.extend_from_slice(message);

    // Pad every message to a fixed envelope size. The cell count is then a
    // function of the envelope class alone (4 KiB or 48 KiB), never of the
    // actual payload length.
    let envelope_len = if inner.len() <= PADDED_ENVELOPE {
        PADDED_ENVELOPE
    } else {
        MAX_ONION_MESSAGE
    };
    if inner.len() > envelope_len || envelope_len > u16::MAX as usize {
        return None;
    }
    let mut padding = vec![0u8; envelope_len - inner.len()];
    OsRng.fill_bytes(&mut padding);
    inner.extend_from_slice(&padding);

    // The signature covers the declared length and the entire padded payload,
    // so a relay cannot truncate the message or rewrite its padding.
    let mut signed = Vec::with_capacity(
        ONION_SIGN_PREFIX.len() + 1 + ID_LEN + 32 + return_bytes.len() + 4 + inner.len()
            - signature_offset,
    );
    signed.extend_from_slice(ONION_SIGN_PREFIX);
    signed.extend_from_slice(b"msg");
    signed.push(kind.to_byte());
    signed.extend_from_slice(&request_id);
    signed.extend_from_slice(&keypair.public.0);
    signed.extend_from_slice(&return_bytes);
    signed.extend_from_slice(&payload_len.to_le_bytes());
    signed.extend_from_slice(&inner[signature_offset + 64..]);
    let signature = keypair.sign(&signed);
    inner[signature_offset..signature_offset + 64].copy_from_slice(&signature.0);

    let count = envelope_len.div_ceil(MAX_CHUNK);
    let mut id = [0u8; ID_LEN];
    OsRng.fill_bytes(&mut id);
    let mut payloads = Vec::with_capacity(count);
    let total = envelope_len as u16;
    for (index, chunk) in inner.chunks(MAX_CHUNK).enumerate() {
        let mut payload = Vec::with_capacity(FRAGMENT_HEADER_LEN + chunk.len());
        payload.extend_from_slice(&FRAGMENT_MAGIC);
        payload.extend_from_slice(&id);
        payload.extend_from_slice(&total.to_le_bytes());
        payload.extend_from_slice(&(index as u16).to_le_bytes());
        payload.extend_from_slice(&(count as u16).to_le_bytes());
        payload.extend_from_slice(chunk);
        payloads.push(payload);
    }
    Some(payloads)
}

/// Verify a reassembled onion message and decode its framing.
pub(crate) fn decode_message(bytes: &[u8]) -> Option<OnionMessage> {
    if bytes.len() < INNER_HEADER_LEN || bytes[0..4] != MESSAGE_MAGIC {
        return None;
    }
    let kind = OnionKind::from_byte(bytes[4])?;
    let mut request_id = [0u8; ID_LEN];
    request_id.copy_from_slice(&bytes[5..21]);
    let mut sender_bytes = [0u8; 32];
    sender_bytes.copy_from_slice(&bytes[21..53]);
    let sender = PublicKey(sender_bytes);
    let return_count = bytes[53] as usize;
    if return_count > MAX_RETURN_HOPS || (kind == OnionKind::Reply && return_count != 0) {
        return None;
    }

    // The signed return-path slice includes the count byte itself.
    let return_start = 53;
    let mut cursor = 54;
    let mut hops: Vec<DhPublicKey> = Vec::with_capacity(return_count);
    let mut first_hop: Option<SocketAddr> = None;
    for index in 0..return_count {
        let (addr, used) = decode_addr(&bytes[cursor..])?;
        cursor += used;
        let key_bytes = bytes.get(cursor..cursor + 32)?;
        cursor += 32;
        let mut key = [0u8; 32];
        key.copy_from_slice(key_bytes);
        if index == 0 {
            first_hop = Some(addr);
        }
        hops.push(DhPublicKey(key));
    }
    let return_end = cursor;
    let payload_len = u32::from_le_bytes(bytes.get(cursor..cursor + 4)?.try_into().ok()?) as usize;
    cursor += 4;
    let signature_bytes = bytes.get(cursor..cursor + 64)?;
    cursor += 64;
    let mut signature = [0u8; 64];
    signature.copy_from_slice(signature_bytes);
    let signature = Signature(signature);
    let padded = bytes.get(cursor..)?;
    if payload_len > padded.len() {
        return None;
    }
    let message = &padded[..payload_len];

    let mut signed = Vec::with_capacity(
        ONION_SIGN_PREFIX.len() + 1 + ID_LEN + 32 + (return_end - return_start) + 4 + padded.len(),
    );
    signed.extend_from_slice(ONION_SIGN_PREFIX);
    signed.extend_from_slice(b"msg");
    signed.push(kind.to_byte());
    signed.extend_from_slice(&request_id);
    signed.extend_from_slice(&sender.0);
    signed.extend_from_slice(&bytes[return_start..return_end]);
    signed.extend_from_slice(&(payload_len as u32).to_le_bytes());
    signed.extend_from_slice(padded);
    if !signature.verify(&sender, &signed) {
        return None;
    }

    let return_path = match (kind, first_hop) {
        (OnionKind::Request, Some(first_hop)) if !hops.is_empty() => Some(OnionReturnPath {
            first_hop,
            path: hops,
        }),
        _ => None,
    };
    Some(OnionMessage {
        kind,
        request_id,
        sender,
        return_path,
        message: message.to_vec(),
    })
}

/// Encode a socket address as `family (1) | address | port (2)`.
fn encode_addr(addr: &SocketAddr, out: &mut Vec<u8>) {
    match addr.ip() {
        IpAddr::V4(ip) => {
            out.push(4);
            out.extend_from_slice(&ip.octets());
            out.extend_from_slice(&addr.port().to_be_bytes());
        }
        IpAddr::V6(ip) => {
            out.push(16);
            out.extend_from_slice(&ip.octets());
            out.extend_from_slice(&addr.port().to_be_bytes());
        }
    }
}

/// Decode one address written by [`encode_addr`]; returns it and the bytes
/// consumed.
fn decode_addr(bytes: &[u8]) -> Option<(SocketAddr, usize)> {
    let family = *bytes.first()?;
    match family {
        4 => {
            let raw = bytes.get(1..7)?;
            let ip = Ipv4Addr::new(raw[0], raw[1], raw[2], raw[3]);
            let port = u16::from_be_bytes([raw[4], raw[5]]);
            Some((SocketAddr::new(IpAddr::V4(ip), port), 7))
        }
        16 => {
            let raw = bytes.get(1..19)?;
            let mut octets = [0u8; 16];
            octets.copy_from_slice(&raw[..16]);
            let port = u16::from_be_bytes([raw[16], raw[17]]);
            Some((
                SocketAddr::new(IpAddr::V6(Ipv6Addr::from(octets)), port),
                19,
            ))
        }
        _ => None,
    }
}

struct Partial {
    total_len: usize,
    chunks: Vec<Option<Vec<u8>>>,
    received: usize,
    buffered: usize,
    created: Instant,
}

/// Bounded reassembly of the `PON1` fragment stream delivered by onion exits.
#[derive(Default)]
pub(crate) struct OnionAssembly {
    partials: HashMap<[u8; ID_LEN], Partial>,
    buffered: usize,
}

impl OnionAssembly {
    /// Feed one exit-hop payload. Returns the signed inner message once every
    /// fragment arrived; duplicate or still-incomplete payloads return `None`.
    pub(crate) fn insert(&mut self, payload: &[u8], now: Instant) -> Option<Vec<u8>> {
        self.expire(now);
        if payload.len() < FRAGMENT_HEADER_LEN || payload[0..4] != FRAGMENT_MAGIC {
            return None;
        }
        let mut id = [0u8; ID_LEN];
        id.copy_from_slice(&payload[4..20]);
        let total_len = u16::from_le_bytes([payload[20], payload[21]]) as usize;
        let index = u16::from_le_bytes([payload[22], payload[23]]) as usize;
        let count = u16::from_le_bytes([payload[24], payload[25]]) as usize;
        if total_len == 0
            || total_len > MAX_ONION_MESSAGE
            || count == 0
            || index >= count
            || count > total_len.div_ceil(MAX_CHUNK)
        {
            return None;
        }
        let chunk = &payload[FRAGMENT_HEADER_LEN..];

        {
            let partial = self.partials.entry(id).or_insert_with(|| Partial {
                total_len,
                chunks: vec![None; count],
                received: 0,
                buffered: 0,
                created: now,
            });
            if partial.total_len != total_len || partial.chunks.len() != count {
                self.remove(&id);
                return None;
            }
            if partial.chunks[index].is_some() {
                return None;
            }
            partial.chunks[index] = Some(chunk.to_vec());
            partial.received += 1;
            partial.buffered += chunk.len();
            self.buffered += chunk.len();
        }

        while self.buffered > MAX_BUFFERED_BYTES || self.partials.len() > MAX_ASSEMBLIES {
            let oldest = self
                .partials
                .iter()
                .min_by_key(|(_, partial)| partial.created)
                .map(|(id, _)| *id)?;
            self.remove(&oldest);
        }

        let partial = self.partials.get(&id)?;
        if partial.received != count {
            return None;
        }
        let mut message = Vec::with_capacity(total_len);
        for chunk in &partial.chunks {
            message.extend_from_slice(chunk.as_ref()?);
        }
        if message.len() != total_len {
            self.remove(&id);
            return None;
        }
        self.remove(&id);
        Some(message)
    }

    fn remove(&mut self, id: &[u8; ID_LEN]) {
        if let Some(partial) = self.partials.remove(id) {
            self.buffered = self.buffered.saturating_sub(partial.buffered);
        }
    }

    fn expire(&mut self, now: Instant) {
        let expired: Vec<[u8; ID_LEN]> = self
            .partials
            .iter()
            .filter(|(_, partial)| now.saturating_duration_since(partial.created) >= ASSEMBLY_TTL)
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            self.remove(&id);
        }
    }
}

/// One relayed onion cell, reported to the node layer so it can meter and mint
/// relay credits (the onion layer itself has no ledger).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayEvent {
    /// Address the cell arrived from (the previous relay).
    pub from: std::net::SocketAddr,
    /// Bytes actually relayed (one fixed-size cell).
    pub bytes: u64,
    /// The exact cell bytes that were peeled, so a credit minter can fold
    /// them into its relay hash chain.
    pub cell: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn return_path() -> OnionReturnPath {
        OnionReturnPath {
            first_hop: "127.0.0.1:4001".parse().unwrap(),
            path: vec![
                DhPublicKey([1; 32]),
                DhPublicKey([2; 32]),
                DhPublicKey([3; 32]),
            ],
        }
    }

    #[test]
    fn message_framing_round_trips_requests_and_replies() {
        let kp = Keypair::generate();
        let mut request_id = [0u8; ID_LEN];
        request_id.copy_from_slice(b"request-id-12345");
        let payload = vec![7u8; 400];

        let chunks = encode_message_with(
            &kp,
            OnionKind::Request,
            request_id,
            Some(&return_path()),
            &payload,
        )
        .expect("request encodes");
        let mut assembly = OnionAssembly::default();
        let now = Instant::now();
        let mut decoded = None;
        for chunk in chunks {
            if let Some(message) = assembly.insert(&chunk, now) {
                decoded = Some(message);
            }
        }
        let decoded = decoded.expect("fragments reassemble");
        let message = decode_message(&decoded).expect("request verifies");
        assert_eq!(message.kind, OnionKind::Request);
        assert_eq!(message.request_id, request_id);
        assert_eq!(message.sender, kp.public);
        assert_eq!(message.message, payload);
        let path = message.return_path.expect("return path present");
        assert_eq!(path.first_hop, return_path().first_hop);
        assert_eq!(path.path, return_path().path);
        assert!(path.is_valid());

        let reply_id = [9u8; ID_LEN];
        let reply_chunks = encode_message_with(
            &kp,
            OnionKind::Reply,
            reply_id,
            Some(&return_path()),
            b"answer",
        )
        .expect("reply encodes");
        let mut assembly = OnionAssembly::default();
        let mut reply = None;
        for payload in reply_chunks {
            if let Some(message) = assembly.insert(&payload, now) {
                reply = Some(message);
            }
        }
        let reply = decode_message(&reply.expect("reply reassembles")).expect("reply verifies");
        assert_eq!(reply.kind, OnionKind::Reply);
        assert_eq!(reply.request_id, reply_id);
        assert_eq!(reply.message, b"answer");
        assert_eq!(reply.return_path, None);
    }

    #[test]
    fn envelopes_pad_multi_cell_messages_to_fixed_cell_counts() {
        let kp = Keypair::generate();
        let path = return_path();
        let encode = |len: usize| {
            encode_message_with(
                &kp,
                OnionKind::Request,
                [5u8; ID_LEN],
                Some(&path),
                &vec![0x5a; len],
            )
            .expect("encodes")
        };
        let reassemble = |chunks: Vec<Vec<u8>>| {
            let mut assembly = OnionAssembly::default();
            let now = Instant::now();
            for chunk in chunks {
                if let Some(message) = assembly.insert(&chunk, now) {
                    return message;
                }
            }
            panic!("fragments reassemble");
        };

        let small_short = encode(32);
        let small_long = encode(3_000);
        let bulk_short = encode(5_000);
        let bulk_long = encode(40_000);

        // Every message in the same size class has the same observable cell
        // count, so an observer cannot fingerprint the payload length.
        assert_eq!(small_short.len(), PADDED_ENVELOPE.div_ceil(MAX_CHUNK));
        assert_eq!(small_short.len(), small_long.len());
        assert_eq!(bulk_short.len(), MAX_ONION_MESSAGE.div_ceil(MAX_CHUNK));
        assert_eq!(bulk_short.len(), bulk_long.len());
        assert_ne!(small_short.len(), bulk_short.len());

        for (chunks, len) in [
            (small_short, 32),
            (small_long, 3_000),
            (bulk_short, 5_000),
            (bulk_long, 40_000),
        ] {
            let message = decode_message(&reassemble(chunks)).expect("verifies");
            assert_eq!(message.message, vec![0x5a; len]);
        }
    }

    #[test]
    fn tampered_framing_is_rejected() {
        let kp = Keypair::generate();
        let chunks = encode_message_with(
            &kp,
            OnionKind::Request,
            [4u8; ID_LEN],
            Some(&return_path()),
            b"hello",
        )
        .expect("encodes");
        let mut assembly = OnionAssembly::default();
        let mut inner = None;
        for payload in chunks {
            if let Some(message) = assembly.insert(&payload, Instant::now()) {
                inner = Some(message);
            }
        }
        let mut inner = inner.expect("reassembles");
        let end = inner.len();
        inner[end - 1] ^= 0xff;
        assert!(decode_message(&inner).is_none());

        // The kind byte is signed, so downgrading a request to a reply (or
        // upgrading a reply) invalidates the originator's signature.
        let mut forged = inner.clone();
        forged[4] = OnionKind::Reply.to_byte();
        assert!(decode_message(&forged).is_none());
    }

    #[test]
    fn return_path_addresses_round_trip_v4_and_v6() {
        for addr in ["127.0.0.1:9", "[::1]:9"] {
            let addr: SocketAddr = addr.parse().unwrap();
            let mut bytes = Vec::new();
            encode_addr(&addr, &mut bytes);
            let (decoded, used) = decode_addr(&bytes).expect("decodes");
            assert_eq!(decoded, addr);
            assert_eq!(used, bytes.len());
        }
    }

    #[test]
    fn oversized_return_path_is_rejected() {
        let kp = Keypair::generate();
        let mut path = return_path();
        path.path = (0..=MAX_RETURN_HOPS as u8)
            .map(|index| DhPublicKey([index; 32]))
            .collect();
        assert!(
            encode_message_with(&kp, OnionKind::Request, [1u8; ID_LEN], Some(&path), b"x",)
                .is_none()
        );
    }
}
