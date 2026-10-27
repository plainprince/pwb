//! Application-level end-to-end encryption flag (`pwb-core::net::e2ee`).
//!
//! Transport envelopes already encrypt every datagram for its recipient, but
//! the *inner* [`NetworkMessage`] is plaintext JSON anywhere it is forwarded:
//! a relay handler, an onion exit, and every application subscriber all read
//! it. This module adds payload-level E2EE as a **send flag** so an app can
//! seal any message for its peer without touching crypto:
//!
//! ```text
//! engine.send_to_with(&peer, message, SendFlags::E2EE)?;
//! ```
//!
//! A sealed message travels as [`NetworkMessage::Sealed`]: a fresh ephemeral
//! X25519 key sealed to the recipient's static key (derived from their
//! Ed25519 identity), with the payload encrypted by ChaCha20-Poly1305 under a
//! BLAKE3-derived per-message key:
//!
//! ```text
//! shared   = X25519(ephemeral_secret, recipient_static_public)
//! msg_key  = BLAKE3-derive_key("pwb-core e2ee v1", shared || salt)
//! aad      = "PWB-E2EE1" || sender || recipient || salt || id
//! ```
//!
//! Properties:
//! - **Confidentiality**: only the holder of the recipient identity key can
//!   recover the plaintext — not relays, onion exits, or observers.
//! - **Sender binding**: the AAD commits to the envelope signer and the
//!   recipient, so a captured ciphertext cannot be re-attributed to another
//!   sender or spliced into a different pair's conversation.
//! - **Forward secrecy per message**: the ephemeral key is discarded after
//!   sealing; compromising a recipient's long-term key later does not
//!   decrypt recorded traffic.
//! - **Replay rejection**: each message carries a random 128-bit id, tracked
//!   in a bounded TTL cache ([`E2eeState`]), so a captured message is
//!   delivered at most once.
//!
//! Sender authentication comes from the existing layers (the envelope's
//! Ed25519 signature or the onion origin signature); E2EE adds payload
//! confidentiality and integrity. The receiver opens the message and
//! dispatches the inner [`NetworkMessage`] transparently, so subscribers keep
//! their existing API. The nonce is fixed because the key itself is unique
//! per message (fresh random 128-bit `salt`).

use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use rand::rngs::OsRng;
use rand::RngCore;

use crate::crypto::{DhKeypair, DhPublicKey, PublicKey};

use super::messages::{NetworkError, NetworkMessage};

/// BLAKE3 `derive_key` context for E2EE payload keys.
const KDF_CONTEXT: &str = "pwb-core e2ee v1";
/// Domain-separation prefix bound into the AEAD's AAD.
const AAD_PREFIX: &[u8; 9] = b"PWB-E2EE1";
/// Salt length (bytes) mixed into the per-message key derivation.
pub const SALT_LEN: usize = 16;
/// Message id length (bytes); the anti-replay cache keys on it.
pub const MSG_ID_LEN: usize = 16;

/// How long a sealed message id stays in the replay cache. A sealed message
/// is never legitimately retransmitted (retries seal a fresh message), so a
/// short TTL comfortably covers network reordering and delayed duplicates.
const REPLAY_TTL: Duration = Duration::from_secs(600);
/// Bounded replay-cache capacity (oldest-first eviction).
const REPLAY_CAPACITY: usize = 8192;

/// Anti-replay state for inbound sealed messages: bounded, TTL'd, keyed by a
/// digest of `(sender, message id)` so one peer's ids cannot evict another's
/// inside the TTL window.
#[derive(Debug)]
pub struct E2eeState {
    seen: Mutex<ReplaySet>,
}

#[derive(Debug)]
struct ReplaySet {
    seen: HashMap<[u8; 32], Instant>,
    order: VecDeque<([u8; 32], Instant)>,
    ttl: Duration,
    capacity: usize,
}

impl ReplaySet {
    fn new(ttl: Duration, capacity: usize) -> Self {
        Self {
            seen: HashMap::new(),
            order: VecDeque::new(),
            ttl,
            capacity: capacity.max(1),
        }
    }

    /// Record `(sender, id)` and report whether it is fresh (`true`) or a
    /// replay still inside the TTL (`false`).
    fn accept(&mut self, digest: [u8; 32], now: Instant) -> bool {
        if let Some(seen_at) = self.seen.get(&digest) {
            if now.saturating_duration_since(*seen_at) <= self.ttl {
                return false;
            }
        }
        self.seen.insert(digest, now);
        self.order.push_back((digest, now));
        while self.order.len() > self.capacity || self.seen.len() > self.capacity {
            let Some((oldest, inserted_at)) = self.order.pop_front() else {
                break;
            };
            // Only remove the current entry when the queue item still owns
            // it; a digest reinserted after its TTL has a newer timestamp.
            if self.seen.get(&oldest) == Some(&inserted_at) {
                self.seen.remove(&oldest);
            }
        }
        true
    }
}

impl Default for E2eeState {
    fn default() -> Self {
        Self {
            seen: Mutex::new(ReplaySet::new(REPLAY_TTL, REPLAY_CAPACITY)),
        }
    }
}

impl E2eeState {
    /// Accept `(sender, id)` at most once inside the TTL window.
    pub fn accept(&self, sender: &PublicKey, id: &[u8; MSG_ID_LEN], now: Instant) -> bool {
        let mut buf = [0u8; 32 + MSG_ID_LEN];
        buf[..32].copy_from_slice(&sender.0);
        buf[32..].copy_from_slice(id);
        let digest = *blake3::hash(&buf).as_bytes();
        self.seen.lock().unwrap().accept(digest, now)
    }
}

fn derive_key(shared: &[u8; 32], salt: &[u8; SALT_LEN]) -> Key {
    let mut material = [0u8; 32 + SALT_LEN];
    material[..32].copy_from_slice(shared);
    material[32..].copy_from_slice(salt);
    *Key::from_slice(&blake3::derive_key(KDF_CONTEXT, &material))
}

fn aad(
    sender: &PublicKey,
    recipient: &PublicKey,
    salt: &[u8; SALT_LEN],
    id: &[u8; MSG_ID_LEN],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(AAD_PREFIX.len() + 32 + 32 + SALT_LEN + MSG_ID_LEN);
    out.extend_from_slice(AAD_PREFIX);
    out.extend_from_slice(&sender.0);
    out.extend_from_slice(&recipient.0);
    out.extend_from_slice(salt);
    out.extend_from_slice(id);
    out
}

/// Seal `inner` for `recipient`, returning the [`NetworkMessage::Sealed`]
/// wrapper. `sender` is the local identity and is bound into the AAD so the
/// ciphertext only opens under that sender/recipient pair.
///
/// This is the primitive behind [`SendFlags::E2EE`](super::SendFlags::E2EE);
/// call it directly to pair E2EE with another send path (for example an
/// onion circuit).
pub fn seal(
    sender: &PublicKey,
    recipient: &PublicKey,
    inner: &NetworkMessage,
) -> Result<NetworkMessage, NetworkError> {
    let recipient_dh = DhPublicKey::from_ed25519(recipient).ok_or_else(|| {
        NetworkError::InvalidConnectionLink(format!(
            "recipient key {} is not a valid curve point",
            recipient.to_hex()
        ))
    })?;

    let mut salt = [0u8; SALT_LEN];
    let mut id = [0u8; MSG_ID_LEN];
    OsRng.fill_bytes(&mut salt);
    OsRng.fill_bytes(&mut id);

    let eph = DhKeypair::generate();
    let shared = eph.diffie_hellman(&recipient_dh);
    let key = derive_key(&shared, &salt);

    let plaintext = serde_json::to_vec(inner).map_err(NetworkError::Serialization)?;
    let ciphertext = ChaCha20Poly1305::new(&key)
        .encrypt(
            &Nonce::default(),
            Payload {
                msg: &plaintext,
                aad: &aad(sender, recipient, &salt, &id),
            },
        )
        .map_err(|_| NetworkError::InvalidMessage)?;

    Ok(NetworkMessage::Sealed {
        eph: eph.public.0,
        salt,
        id,
        ciphertext,
    })
}

/// Open a sealed message with the local identity. Returns the inner
/// [`NetworkMessage`], or `None` when `sealed` is not
/// [`NetworkMessage::Sealed`], when the ciphertext does not authenticate
/// under this sender/recipient pair, when the message id was already
/// delivered (replay), or when the payload itself is another sealed message
/// (nesting is rejected so one unwrap always reaches the app payload).
pub fn open(
    local_key: &PublicKey,
    local_dh: &DhKeypair,
    sender: &PublicKey,
    sealed: &NetworkMessage,
    state: &E2eeState,
) -> Option<NetworkMessage> {
    let NetworkMessage::Sealed {
        eph,
        salt,
        id,
        ciphertext,
    } = sealed
    else {
        return None;
    };
    if !state.accept(sender, id, Instant::now()) {
        return None;
    }
    let shared = local_dh.diffie_hellman(&DhPublicKey(*eph));
    let key = derive_key(&shared, salt);
    let plaintext = ChaCha20Poly1305::new(&key)
        .decrypt(
            &Nonce::default(),
            Payload {
                msg: ciphertext,
                aad: &aad(sender, local_key, salt, id),
            },
        )
        .ok()?;
    let inner: NetworkMessage = serde_json::from_slice(&plaintext).ok()?;
    if matches!(inner, NetworkMessage::Sealed { .. }) {
        return None;
    }
    Some(inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keypair(seed: u8) -> crate::crypto::Keypair {
        crate::crypto::Keypair::from_seed(&[seed; 32])
    }

    #[test]
    fn seal_open_round_trips_the_inner_message() {
        let alice = keypair(1);
        let bob = keypair(2);
        let inner = NetworkMessage::Chat {
            sender: alice.public,
            id: [7u8; 16],
            sent_at: 1_700_000_000,
            text: "hello over e2ee".to_string(),
        };
        let sealed = seal(&alice.public, &bob.public, &inner).expect("seal");

        let state = E2eeState::default();
        let bob_dh = DhKeypair::from_ed25519(&bob);
        let opened = open(&bob.public, &bob_dh, &alice.public, &sealed, &state);
        assert_eq!(opened, Some(inner));
    }

    #[test]
    fn only_the_recipient_identity_can_open() {
        let alice = keypair(1);
        let bob = keypair(2);
        let eve = keypair(3);
        let inner = NetworkMessage::Ping { nonce: 42 };
        let sealed = seal(&alice.public, &bob.public, &inner).expect("seal");

        let eve_dh = DhKeypair::from_ed25519(&eve);
        let state = E2eeState::default();
        assert!(
            open(&eve.public, &eve_dh, &alice.public, &sealed, &state).is_none(),
            "a non-recipient must not open the payload"
        );
    }

    #[test]
    fn tampered_ciphertext_is_rejected() {
        let alice = keypair(1);
        let bob = keypair(2);
        let sealed = seal(
            &alice.public,
            &bob.public,
            &NetworkMessage::Ping { nonce: 1 },
        )
        .expect("seal");
        let NetworkMessage::Sealed {
            eph,
            salt,
            id,
            mut ciphertext,
        } = sealed
        else {
            panic!("seal() returns a Sealed wrapper");
        };
        let last = ciphertext.len() - 1;
        ciphertext[last] ^= 0x40;
        let tampered = NetworkMessage::Sealed {
            eph,
            salt,
            id,
            ciphertext,
        };

        let bob_dh = DhKeypair::from_ed25519(&bob);
        let state = E2eeState::default();
        assert!(
            open(&bob.public, &bob_dh, &alice.public, &tampered, &state).is_none(),
            "bit-flipped ciphertext must fail the AEAD tag"
        );
    }

    #[test]
    fn ciphertext_cannot_be_reattributed_to_another_sender() {
        let alice = keypair(1);
        let bob = keypair(2);
        let mallory = keypair(3);
        let sealed = seal(
            &alice.public,
            &bob.public,
            &NetworkMessage::Ping { nonce: 9 },
        )
        .expect("seal");

        let bob_dh = DhKeypair::from_ed25519(&bob);
        let state = E2eeState::default();
        assert!(
            open(&bob.public, &bob_dh, &mallory.public, &sealed, &state).is_none(),
            "the AAD binds the sender, so re-attribution must fail"
        );
    }

    #[test]
    fn a_replayed_message_id_is_delivered_only_once() {
        let alice = keypair(1);
        let bob = keypair(2);
        let sealed = seal(
            &alice.public,
            &bob.public,
            &NetworkMessage::Ping { nonce: 5 },
        )
        .expect("seal");

        let bob_dh = DhKeypair::from_ed25519(&bob);
        let state = E2eeState::default();
        assert!(
            open(&bob.public, &bob_dh, &alice.public, &sealed, &state).is_some(),
            "first delivery succeeds"
        );
        assert!(
            open(&bob.public, &bob_dh, &alice.public, &sealed, &state).is_none(),
            "the same id must not be delivered twice"
        );
    }

    #[test]
    fn nested_sealed_payloads_are_rejected() {
        let alice = keypair(1);
        let bob = keypair(2);
        let carol = keypair(3);
        // Inner sealed for bob, then wrapped again by alice as if it were a
        // plain payload addressed to bob: one unwrap must reach the app
        // message, never another Sealed layer.
        let inner = seal(
            &alice.public,
            &bob.public,
            &NetworkMessage::Ping { nonce: 1 },
        )
        .expect("inner");
        let outer = seal(&bob.public, &carol.public, &inner).expect("outer");

        // `outer` was created by bob for carol: open it as carol.
        let carol_dh = DhKeypair::from_ed25519(&carol);
        let state = E2eeState::default();
        assert!(
            open(&carol.public, &carol_dh, &bob.public, &outer, &state).is_none(),
            "a sealed-inside-sealed payload must be rejected"
        );
    }

    #[test]
    fn replay_cache_evicts_oldest_at_capacity() {
        let state = E2eeState {
            seen: Mutex::new(ReplaySet::new(REPLAY_TTL, 2)),
        };
        let sender = PublicKey([9u8; 32]);
        assert!(state.accept(&sender, &[1u8; 16], Instant::now()));
        assert!(state.accept(&sender, &[2u8; 16], Instant::now()));
        assert!(state.accept(&sender, &[3u8; 16], Instant::now()));
        // Entries 2 and 3 are still inside the window; entry 1 was evicted
        // to stay within capacity, so it is accepted again as fresh.
        assert!(!state.accept(&sender, &[2u8; 16], Instant::now()));
        assert!(!state.accept(&sender, &[3u8; 16], Instant::now()));
        assert!(state.accept(&sender, &[1u8; 16], Instant::now()));
    }
}
