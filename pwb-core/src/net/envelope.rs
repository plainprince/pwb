//! Authenticated, encrypted datagram envelope, fragmentation, and replay
//! protection.
//!
//! Every datagram the engine sends is signed and encrypted for one recipient:
//!
//! ```text
//! header (114 bytes) | signature (64 bytes) | ciphertext (payload + 16-byte tag)
//! ```
//!
//! ```text
//!   0..8   magic "PWBNET02" (version 2)
//!   8      flags: bit 0 = fragmented
//!   9      reserved (0)
//!  10..12  fragment index
//!  12..14  fragment count
//!  14..18  total plaintext message length (all fragments concatenated)
//!  18..22  ciphertext length of this fragment
//!  22..30  sequence number (monotonic per sender)
//!  30..38  unix timestamp
//!  38..70  sender Ed25519 public key
//!  70..102 per-datagram ephemeral X25519 public key
//! 102..114 ChaCha20-Poly1305 nonce
//! ```
//!
//! The 64-byte Ed25519 signature covers the header followed by the ciphertext,
//! so sender identity, sequence, timestamp, ephemeral key, nonce, and contents
//! are all authenticated. Receivers attribute every datagram to the key that
//! signed it (never to a field inside the JSON payload) and reject
//! duplicates/replays with a per-sender sliding window.
//!
//! ## Confidentiality
//!
//! The sender derives an X25519 key from the recipient's Ed25519 identity key
//! (both curves are birationally equivalent) and generates a fresh ephemeral
//! X25519 keypair for every datagram:
//!
//! ```text
//! shared = X25519(ephemeral_secret, recipient_montgomery_public)
//! key    = BLAKE3-derive_key("pwb-core transport v2", shared)
//! ```
//!
//! The payload is sealed with ChaCha20-Poly1305 under a random 96-bit nonce.
//! Only the holder of the recipient identity can recover the plaintext; an
//! on-path observer sees fixed-size framing and ciphertext. The signature still
//! lets the recipient authenticate the *sender* before decrypting.
//!
//! This is transport encryption, not onion routing: the recipient (and anyone
//! it forwards to) sees the true source address and the sender identity. Onion
//! cells are a separate layer handled by [`crate::onion`].

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use rand::rngs::OsRng;
use rand::RngCore;

use crate::crypto::{DhKeypair, DhPublicKey, Keypair, PublicKey, Signature};

/// Envelope magic prefix, shared by every wire version.
const MAGIC_PREFIX: [u8; 6] = *b"PWBNET";
/// Wire version carried in the last two magic bytes (ASCII digits).
/// Bumping this rejects older nodes cleanly — and the decoder reports the
/// mismatch instead of treating it as random noise.
pub(crate) const WIRE_VERSION: u16 = 2;
/// Full magic for [`WIRE_VERSION`].
const MAGIC: [u8; 8] = *b"PWBNET02";
/// Length of the signed header.
const HEADER_LEN: usize = 114;
/// Length of the Ed25519 signature appended to the header.
const SIG_LEN: usize = 64;
/// Header plus signature, i.e. the per-datagram framing overhead.
const PREFIX_LEN: usize = HEADER_LEN + SIG_LEN;
/// Ephemeral X25519 public key embedded in the header.
const EPH_LEN: usize = 32;
/// ChaCha20-Poly1305 nonce length.
const NONCE_LEN: usize = 12;
/// ChaCha20-Poly1305 authentication tag length.
const TAG_LEN: usize = 16;

/// Domain separation for transport payload keys.
const KDF_TRANSPORT: &str = "pwb-core transport v2";

/// Datagrams are capped well below the smallest common MTU so envelopes are
/// not IP-fragmented. Larger messages are split across multiple datagrams.
pub(crate) const MAX_DATAGRAM_SIZE: usize = 1200;
/// Largest plaintext message accepted after reassembly.
pub(crate) const MAX_MESSAGE_SIZE: usize = 4 * 1024 * 1024;
/// Per-fragment plaintext capacity (ciphertext adds a 16-byte tag).
const FRAGMENT_PAYLOAD: usize = MAX_DATAGRAM_SIZE - PREFIX_LEN - TAG_LEN;
/// Maximum fragment count implied by [`MAX_MESSAGE_SIZE`].
const MAX_FRAGMENTS: usize = MAX_MESSAGE_SIZE.div_ceil(FRAGMENT_PAYLOAD);
/// How long incomplete reassembly state is kept.
const REASSEMBLY_TTL: Duration = Duration::from_secs(30);
/// Maximum bytes buffered across all in-progress reassemblies.
const MAX_BUFFERED_BYTES: usize = 8 * 1024 * 1024;
/// How far ahead of the local clock a sender's timestamp may be.
const MAX_CLOCK_SKEW: u64 = 600;

const FLAG_FRAGMENTED: u8 = 0x01;

/// Why a datagram could not be accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EnvelopeError {
    /// Datagram shorter than the fixed framing.
    TooShort,
    /// Not a PlainWeb network datagram (bad magic).
    BadMagic,
    /// PlainWeb traffic from an incompatible wire version.
    UnsupportedVersion(u16),
    /// Unknown flag bits or inconsistent fragmentation fields.
    BadFraming,
    /// Declared payload length does not match the datagram.
    BadLength,
    /// Message or fragment count exceeds the size policy.
    TooLarge,
    /// Timestamp too far in the future (clock skew or forged envelope).
    ClockSkew,
    /// Signature does not verify under the claimed sender key.
    BadSignature,
    /// AEAD decryption failed (wrong recipient or tampered ciphertext).
    BadCiphertext,
}

/// A decoded, signature-verified, decrypted datagram.
#[derive(Debug, Clone)]
pub(crate) struct Envelope {
    pub sender: PublicKey,
    pub seq: u64,
    /// `None` for unfragmented messages.
    pub fragment: Option<(u16, u16)>,
    /// Total length of the reassembled plaintext message.
    pub total_len: usize,
    pub payload: Vec<u8>,
}

fn put_u16(buf: &mut [u8], at: usize, value: u16) {
    buf[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(buf: &mut [u8], at: usize, value: u32) {
    buf[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn read_u16(buf: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([buf[at], buf[at + 1]])
}

fn read_u32(buf: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]])
}

fn read_u64(buf: &[u8], at: usize) -> u64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&buf[at..at + 8]);
    u64::from_le_bytes(bytes)
}

/// Where one fragment sits within its message.
struct FragmentMeta {
    index: u16,
    count: u16,
    total_len: usize,
    ciphertext_len: usize,
}

#[allow(clippy::too_many_arguments)]
fn write_header(
    header: &mut [u8; HEADER_LEN],
    sender: &PublicKey,
    seq: u64,
    timestamp: u64,
    ephemeral: &DhPublicKey,
    nonce: &[u8; NONCE_LEN],
    fragment: FragmentMeta,
) {
    header[0..8].copy_from_slice(&MAGIC);
    header[8] = if fragment.count > 1 {
        FLAG_FRAGMENTED
    } else {
        0
    };
    header[9] = 0;
    put_u16(header, 10, fragment.index);
    put_u16(header, 12, fragment.count);
    put_u32(header, 14, fragment.total_len as u32);
    put_u32(header, 18, fragment.ciphertext_len as u32);
    header[22..30].copy_from_slice(&seq.to_le_bytes());
    header[30..38].copy_from_slice(&timestamp.to_le_bytes());
    header[38..70].copy_from_slice(&sender.0);
    header[70..70 + EPH_LEN].copy_from_slice(&ephemeral.0);
    header[70 + EPH_LEN..70 + EPH_LEN + NONCE_LEN].copy_from_slice(nonce);
}

fn derive_transport_key(shared: &[u8; 32]) -> [u8; 32] {
    blake3::derive_key(KDF_TRANSPORT, shared)
}

/// Serialize and encrypt `message` for `recipient` into one or more signed
/// datagrams.
///
/// `seq` must be unique and monotonic per sender; receivers use it to reject
/// replays. All fragments of one message share the same sequence number.
pub(crate) fn encode_for(
    keypair: &Keypair,
    recipient: &DhPublicKey,
    seq: u64,
    timestamp: u64,
    message: &[u8],
) -> Result<Vec<Vec<u8>>, EnvelopeError> {
    if message.len() > MAX_MESSAGE_SIZE {
        return Err(EnvelopeError::TooLarge);
    }
    let count = message.len().div_ceil(FRAGMENT_PAYLOAD).max(1);
    if count > MAX_FRAGMENTS || count > u16::MAX as usize {
        return Err(EnvelopeError::TooLarge);
    }
    let count_u16 = count as u16;

    let mut datagrams = Vec::with_capacity(count);
    for (index, chunk) in message.chunks(FRAGMENT_PAYLOAD).enumerate() {
        let ephemeral = DhKeypair::generate();
        let shared = ephemeral.diffie_hellman(recipient);
        let key = derive_transport_key(&shared);
        let mut nonce = [0u8; NONCE_LEN];
        OsRng.fill_bytes(&mut nonce);
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
        let ciphertext = cipher
            .encrypt(Nonce::from_slice(&nonce), chunk)
            .map_err(|_| EnvelopeError::BadCiphertext)?;

        let mut header = [0u8; HEADER_LEN];
        write_header(
            &mut header,
            &keypair.public,
            seq,
            timestamp,
            &ephemeral.public,
            &nonce,
            FragmentMeta {
                index: index as u16,
                count: count_u16,
                total_len: message.len(),
                ciphertext_len: ciphertext.len(),
            },
        );

        // Sign header || ciphertext so every framing field is authenticated.
        let mut signed = Vec::with_capacity(HEADER_LEN + ciphertext.len());
        signed.extend_from_slice(&header);
        signed.extend_from_slice(&ciphertext);
        let signature = keypair.sign(&signed);

        let mut datagram = Vec::with_capacity(PREFIX_LEN + ciphertext.len());
        datagram.extend_from_slice(&header);
        datagram.extend_from_slice(&signature.0);
        datagram.extend_from_slice(&ciphertext);
        datagrams.push(datagram);
    }

    // An empty message still needs one datagram to travel.
    if datagrams.is_empty() {
        datagrams.push(encode_empty(keypair, recipient, seq, timestamp)?);
    }

    Ok(datagrams)
}

fn encode_empty(
    keypair: &Keypair,
    recipient: &DhPublicKey,
    seq: u64,
    timestamp: u64,
) -> Result<Vec<u8>, EnvelopeError> {
    let ephemeral = DhKeypair::generate();
    let shared = ephemeral.diffie_hellman(recipient);
    let key = derive_transport_key(&shared);
    let mut nonce = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce);
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), &[][..])
        .map_err(|_| EnvelopeError::BadCiphertext)?;

    let mut header = [0u8; HEADER_LEN];
    write_header(
        &mut header,
        &keypair.public,
        seq,
        timestamp,
        &ephemeral.public,
        &nonce,
        FragmentMeta {
            index: 0,
            count: 1,
            total_len: 0,
            ciphertext_len: ciphertext.len(),
        },
    );
    let mut signed = Vec::with_capacity(HEADER_LEN + ciphertext.len());
    signed.extend_from_slice(&header);
    signed.extend_from_slice(&ciphertext);
    let signature = keypair.sign(&signed);

    let mut datagram = Vec::with_capacity(PREFIX_LEN + ciphertext.len());
    datagram.extend_from_slice(&header);
    datagram.extend_from_slice(&signature.0);
    datagram.extend_from_slice(&ciphertext);
    Ok(datagram)
}

/// Decode, verify, and decrypt one datagram for `recipient`. `now_unix` bounds
/// forward clock skew.
pub(crate) fn decode_for(
    datagram: &[u8],
    recipient: &DhKeypair,
    now_unix: u64,
) -> Result<Envelope, EnvelopeError> {
    if datagram.len() < PREFIX_LEN + TAG_LEN {
        return Err(EnvelopeError::TooShort);
    }
    if datagram.len() > MAX_DATAGRAM_SIZE {
        return Err(EnvelopeError::TooLarge);
    }
    if datagram[0..MAGIC_PREFIX.len()] != MAGIC_PREFIX {
        return Err(EnvelopeError::BadMagic);
    }
    if datagram[0..8] != MAGIC {
        // Same protocol, different generation: the peer is not garbage, it is
        // out of date. Report the version it speaks so the operator can fix it.
        let version = std::str::from_utf8(&datagram[MAGIC_PREFIX.len()..8])
            .ok()
            .and_then(|digits| digits.parse::<u16>().ok())
            .unwrap_or(0);
        return Err(EnvelopeError::UnsupportedVersion(version));
    }
    let flags = datagram[8];
    if flags & !FLAG_FRAGMENTED != 0 {
        return Err(EnvelopeError::BadFraming);
    }

    let index = read_u16(datagram, 10);
    let count = read_u16(datagram, 12);
    let total_len = read_u32(datagram, 14) as usize;
    let ciphertext_len = read_u32(datagram, 18) as usize;
    let seq = read_u64(datagram, 22);
    let timestamp = read_u64(datagram, 30);

    let mut sender = [0u8; 32];
    sender.copy_from_slice(&datagram[38..70]);
    let sender = PublicKey(sender);

    let mut ephemeral = [0u8; EPH_LEN];
    ephemeral.copy_from_slice(&datagram[70..70 + EPH_LEN]);
    let ephemeral = DhPublicKey::from_bytes(ephemeral);

    let mut nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(&datagram[70 + EPH_LEN..70 + EPH_LEN + NONCE_LEN]);

    let fragmented = flags & FLAG_FRAGMENTED != 0;
    if count == 0 || (fragmented && count < 2) || (!fragmented && count != 1) {
        return Err(EnvelopeError::BadFraming);
    }
    if index >= count || count as usize > MAX_FRAGMENTS {
        return Err(EnvelopeError::BadFraming);
    }
    if total_len > MAX_MESSAGE_SIZE {
        return Err(EnvelopeError::TooLarge);
    }
    if PREFIX_LEN + ciphertext_len != datagram.len() {
        return Err(EnvelopeError::BadLength);
    }
    if ciphertext_len < TAG_LEN {
        return Err(EnvelopeError::BadLength);
    }
    let plaintext_len = ciphertext_len - TAG_LEN;
    if plaintext_len > total_len {
        return Err(EnvelopeError::BadFraming);
    }
    if !fragmented && total_len != plaintext_len {
        return Err(EnvelopeError::BadFraming);
    }
    if timestamp > now_unix.saturating_add(MAX_CLOCK_SKEW) {
        return Err(EnvelopeError::ClockSkew);
    }

    let mut signature_bytes = [0u8; SIG_LEN];
    signature_bytes.copy_from_slice(&datagram[HEADER_LEN..PREFIX_LEN]);
    let signature = Signature(signature_bytes);

    let mut signed = Vec::with_capacity(HEADER_LEN + ciphertext_len);
    signed.extend_from_slice(&datagram[..HEADER_LEN]);
    signed.extend_from_slice(&datagram[PREFIX_LEN..]);
    if !signature.verify(&sender, &signed) {
        return Err(EnvelopeError::BadSignature);
    }

    let shared = recipient.diffie_hellman(&ephemeral);
    let key = derive_transport_key(&shared);
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
    let payload = cipher
        .decrypt(
            Nonce::from_slice(&nonce),
            &datagram[PREFIX_LEN..PREFIX_LEN + ciphertext_len],
        )
        .map_err(|_| EnvelopeError::BadCiphertext)?;

    Ok(Envelope {
        sender,
        seq,
        fragment: if fragmented {
            Some((index, count))
        } else {
            None
        },
        total_len,
        payload,
    })
}

/// A fully reassembled inbound message.
#[derive(Debug)]
pub(crate) struct InboundMessage {
    pub sender: PublicKey,
    pub seq: u64,
    pub from: SocketAddr,
    pub bytes: Vec<u8>,
}

struct Partial {
    count: u16,
    total_len: usize,
    chunks: Vec<Option<Vec<u8>>>,
    received: u16,
    buffered: usize,
    created: Instant,
    from: SocketAddr,
}

/// Bounded fragment reassembly for inbound datagrams.
#[derive(Default)]
pub(crate) struct Reassembler {
    partials: HashMap<(PublicKey, u64), Partial>,
    buffered: usize,
}

impl Reassembler {
    /// Feed one verified fragment. Returns the message once every fragment has
    /// arrived; duplicate or still-incomplete fragments return `None`.
    pub(crate) fn insert(
        &mut self,
        envelope: &Envelope,
        from: SocketAddr,
        now: Instant,
    ) -> Option<InboundMessage> {
        self.expire(now);

        let Some((index, count)) = envelope.fragment else {
            return Some(InboundMessage {
                sender: envelope.sender,
                seq: envelope.seq,
                from,
                bytes: envelope.payload.clone(),
            });
        };

        let key = (envelope.sender, envelope.seq);

        // A fragment whose framing disagrees with the first one we saw for
        // this (sender, seq) is bogus; drop the whole assembly rather than
        // mix shapes.
        if let Some(existing) = self.partials.get(&key) {
            if existing.count != count || existing.total_len != envelope.total_len {
                self.remove(&key);
                return None;
            }
        }

        let mut added = 0usize;
        let mut duplicate = false;
        {
            let partial = self.partials.entry(key).or_insert_with(|| Partial {
                count,
                total_len: envelope.total_len,
                chunks: vec![None; count as usize],
                received: 0,
                buffered: 0,
                created: now,
                from,
            });
            let slot = partial.chunks.get_mut(index as usize)?;
            if slot.is_some() {
                duplicate = true;
            } else {
                let len = envelope.payload.len();
                *slot = Some(envelope.payload.clone());
                partial.received += 1;
                partial.buffered += len;
                added = len;
            }
        }
        if duplicate {
            return None;
        }
        self.buffered += added;

        // Enforce the global buffer budget (evict the oldest assemblies).
        while self.buffered > MAX_BUFFERED_BYTES {
            let oldest = self
                .partials
                .iter()
                .min_by_key(|(_, p)| p.created)
                .map(|(key, _)| *key)?;
            self.remove(&oldest);
        }

        let complete = {
            let partial = self.partials.get(&key)?;
            partial.received == partial.count
        };
        if !complete {
            return None;
        }

        let partial = self.partials.remove(&key)?;
        self.buffered = self.buffered.saturating_sub(partial.buffered);

        let mut bytes = Vec::with_capacity(partial.total_len);
        for chunk in partial.chunks {
            bytes.extend_from_slice(&chunk?);
        }
        if bytes.len() != partial.total_len {
            return None;
        }

        Some(InboundMessage {
            sender: envelope.sender,
            seq: envelope.seq,
            from: partial.from,
            bytes,
        })
    }

    fn remove(&mut self, key: &(PublicKey, u64)) {
        if let Some(partial) = self.partials.remove(key) {
            self.buffered = self.buffered.saturating_sub(partial.buffered);
        }
    }

    fn expire(&mut self, now: Instant) {
        let expired: Vec<(PublicKey, u64)> = self
            .partials
            .iter()
            .filter(|(_, p)| now.saturating_duration_since(p.created) >= REASSEMBLY_TTL)
            .map(|(key, _)| *key)
            .collect();
        for key in expired {
            self.remove(&key);
        }
    }
}

/// Upper bound on tracked senders, so an attacker cycling identities cannot
/// grow the replay table without bound.
const MAX_TRACKED_SENDERS: usize = 8192;

/// Sliding-window replay protection, keyed by authenticated sender.
#[derive(Default)]
pub(crate) struct ReplayGuard {
    windows: HashMap<PublicKey, (u64, u64)>,
}

impl ReplayGuard {
    /// Accept `seq` once per sender. Out-of-order delivery within the last 64
    /// sequence numbers is allowed; anything older or repeated is rejected.
    pub(crate) fn accept(&mut self, sender: &PublicKey, seq: u64) -> bool {
        if seq == 0 {
            return false;
        }
        if !self.windows.contains_key(sender) && self.windows.len() >= MAX_TRACKED_SENDERS {
            // Evict an arbitrary sender's window rather than refusing all new
            // senders once the table is full.
            if let Some(evicted) = self.windows.keys().next().copied() {
                self.windows.remove(&evicted);
            }
        }
        let entry = self
            .windows
            .entry(*sender)
            .or_insert((seq.saturating_sub(1), 0));
        let (highest, seen) = *entry;

        if seq > highest {
            let shift = seq - highest;
            let seen = if shift >= 64 { 0 } else { seen << shift };
            *entry = (seq, seen | 1);
            return true;
        }

        let delta = highest - seq;
        if delta >= 64 {
            return false;
        }
        let bit = 1u64 << delta;
        if seen & bit != 0 {
            return false;
        }
        *entry = (highest, seen | bit);
        true
    }

    /// Number of senders currently tracked (test/diagnostic helper).
    #[cfg(test)]
    pub(crate) fn tracked(&self) -> usize {
        self.windows.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;

    fn addr() -> SocketAddr {
        "127.0.0.1:9000".parse().unwrap()
    }

    fn recipient() -> (Keypair, DhKeypair) {
        let kp = Keypair::generate();
        let dh = DhKeypair::from_ed25519(&kp);
        (kp, dh)
    }

    #[test]
    fn single_datagram_roundtrip_verifies_sender_and_encrypts() {
        let sender = Keypair::generate();
        let (_recipient_kp, recipient_dh) = recipient();
        let datagrams = encode_for(
            &sender,
            &recipient_dh.public,
            7,
            1_700_000_000,
            b"hello world",
        )
        .unwrap();
        assert_eq!(datagrams.len(), 1);

        let envelope = decode_for(&datagrams[0], &recipient_dh, 1_700_000_000).unwrap();
        assert_eq!(envelope.sender, sender.public);
        assert_eq!(envelope.seq, 7);
        assert_eq!(envelope.fragment, None);
        assert_eq!(envelope.total_len, 11);
        assert_eq!(envelope.payload, b"hello world");
        // Ciphertext on the wire does not contain the plaintext.
        assert!(!datagrams[0]
            .windows(b"hello world".len())
            .any(|w| w == b"hello world"));
    }

    #[test]
    fn tampering_with_header_or_payload_is_rejected() {
        let sender = Keypair::generate();
        let (_recipient_kp, recipient_dh) = recipient();
        let datagrams =
            encode_for(&sender, &recipient_dh.public, 1, 1_700_000_000, b"payload").unwrap();

        let mut tampered = datagrams[0].clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 0xFF;
        assert_eq!(
            decode_for(&tampered, &recipient_dh, 1_700_000_000).unwrap_err(),
            EnvelopeError::BadSignature
        );

        let mut tampered = datagrams[0].clone();
        tampered[22] ^= 0x01; // sequence number is signed
        assert_eq!(
            decode_for(&tampered, &recipient_dh, 1_700_000_000).unwrap_err(),
            EnvelopeError::BadSignature
        );

        let mut tampered = datagrams[0].clone();
        tampered[38] ^= 0x01; // sender key is signed
        assert_eq!(
            decode_for(&tampered, &recipient_dh, 1_700_000_000).unwrap_err(),
            EnvelopeError::BadSignature
        );
    }

    #[test]
    fn wrong_recipient_cannot_decrypt() {
        let sender = Keypair::generate();
        let (_recipient_kp, recipient_dh) = recipient();
        let (_other_kp, other_dh) = recipient();
        let datagrams =
            encode_for(&sender, &recipient_dh.public, 3, 1_700_000_000, b"secret").unwrap();
        assert_eq!(
            decode_for(&datagrams[0], &other_dh, 1_700_000_000).unwrap_err(),
            EnvelopeError::BadCiphertext
        );
    }

    #[test]
    fn framing_errors_are_rejected() {
        let sender = Keypair::generate();
        let (_recipient_kp, recipient_dh) = recipient();
        let datagrams =
            encode_for(&sender, &recipient_dh.public, 1, 1_700_000_000, b"payload").unwrap();
        let datagram = &datagrams[0];

        assert_eq!(
            decode_for(&datagram[..PREFIX_LEN - 1], &recipient_dh, 1_700_000_000).unwrap_err(),
            EnvelopeError::TooShort
        );

        let mut bad_magic = datagram.clone();
        bad_magic[0] = b'X';
        assert_eq!(
            decode_for(&bad_magic, &recipient_dh, 1_700_000_000).unwrap_err(),
            EnvelopeError::BadMagic
        );

        // Same protocol, newer generation: the version is reported, not
        // silently lumped in with random noise.
        let mut newer_wire = datagram.clone();
        newer_wire[6] = b'0';
        newer_wire[7] = b'3';
        assert_eq!(
            decode_for(&newer_wire, &recipient_dh, 1_700_000_000).unwrap_err(),
            EnvelopeError::UnsupportedVersion(3)
        );

        let mut trailing = datagram.clone();
        trailing.push(0);
        assert_eq!(
            decode_for(&trailing, &recipient_dh, 1_700_000_000).unwrap_err(),
            EnvelopeError::BadLength
        );

        // A datagram larger than the policy is rejected before parsing.
        let mut oversized = datagram.clone();
        oversized.resize(MAX_DATAGRAM_SIZE + 1, 0);
        assert_eq!(
            decode_for(&oversized, &recipient_dh, 1_700_000_000).unwrap_err(),
            EnvelopeError::TooLarge
        );

        assert_eq!(
            decode_for(datagram, &recipient_dh, 1_700_000_000 - MAX_CLOCK_SKEW - 1).unwrap_err(),
            EnvelopeError::ClockSkew
        );
    }

    #[test]
    fn large_messages_fragment_and_reassemble_out_of_order() {
        let sender = Keypair::generate();
        let (_recipient_kp, recipient_dh) = recipient();
        let message = vec![0xA5u8; FRAGMENT_PAYLOAD * 3 + 17];
        let datagrams =
            encode_for(&sender, &recipient_dh.public, 9, 1_700_000_000, &message).unwrap();
        assert!(datagrams.len() > 1);
        assert!(datagrams.iter().all(|d| d.len() <= MAX_DATAGRAM_SIZE));

        let now = Instant::now();
        let mut reassembler = Reassembler::default();

        // Deliver in reverse; nothing completes until the first fragment lands.
        let mut completed = None;
        for datagram in datagrams.iter().rev() {
            let envelope = decode_for(datagram, &recipient_dh, 1_700_000_000).unwrap();
            if let Some(message) = reassembler.insert(&envelope, addr(), now) {
                completed = Some(message);
            }
        }
        let completed = completed.expect("all fragments complete the message");
        assert_eq!(completed.sender, sender.public);
        assert_eq!(completed.seq, 9);
        assert_eq!(completed.bytes, message);
        assert_eq!(completed.from, addr());
    }

    #[test]
    fn empty_message_still_sends_one_datagram() {
        let sender = Keypair::generate();
        let (_recipient_kp, recipient_dh) = recipient();
        let datagrams = encode_for(&sender, &recipient_dh.public, 4, 1_700_000_000, b"").unwrap();
        assert_eq!(datagrams.len(), 1);
        let envelope = decode_for(&datagrams[0], &recipient_dh, 1_700_000_000).unwrap();
        assert!(envelope.payload.is_empty());
        assert_eq!(envelope.seq, 4);
    }

    #[test]
    fn replay_guard_accepts_out_of_order_but_not_repeats() {
        let mut guard = ReplayGuard::default();
        let sender = Keypair::generate().public;

        assert!(guard.accept(&sender, 2));
        assert!(guard.accept(&sender, 1));
        assert!(!guard.accept(&sender, 2));
        assert!(!guard.accept(&sender, 1));

        // A much newer sequence number advances the window and re-allows the
        // recent out-of-order range while the old entries fall out.
        assert!(guard.accept(&sender, 100));
        assert!(!guard.accept(&sender, 1));
        assert!(guard.accept(&sender, 99));

        // The window is per-sender.
        let other = Keypair::generate().public;
        assert!(guard.accept(&other, 1));
        assert_eq!(guard.tracked(), 2);
    }
}
