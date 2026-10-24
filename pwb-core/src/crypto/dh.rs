//! X25519 Diffie-Hellman key agreement.

use std::fmt;

use curve25519_dalek::edwards::CompressedEdwardsY;
use ed25519_dalek::SigningKey;
use rand::rngs::OsRng;
use rand::RngCore;

use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret};

use super::identity::{Keypair, PublicKey};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// X25519 Diffie-Hellman public key (32 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DhPublicKey(pub [u8; 32]);

impl DhPublicKey {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Derive the X25519 public key corresponding to an Ed25519 identity key.
    ///
    /// Ed25519 and X25519 use birationally equivalent curves; a valid Ed25519
    /// verifying key maps to a Montgomery point that peers can use for ECDH.
    /// This lets the transport encrypt to a peer knowing only the identity key
    /// advertised in connection links and peer-exchange messages. Invalid
    /// (non-decompressible) keys return `None`.
    pub fn from_ed25519(public: &PublicKey) -> Option<Self> {
        let point = CompressedEdwardsY(public.0).decompress()?;
        Some(Self(point.to_montgomery().to_bytes()))
    }

    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{:02x}", b)).collect()
    }
}

impl fmt::Display for DhPublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}

/// X25519 Diffie-Hellman secret key (32 bytes).
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct DhSecretKey(pub [u8; 32]);

impl DhSecretKey {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn to_bytes(&self) -> [u8; 32] {
        self.0
    }
}

impl fmt::Debug for DhSecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DhSecretKey(redacted)")
    }
}

/// X25519 Diffie-Hellman keypair for ECDH key agreement.
#[derive(Debug, Clone)]
pub struct DhKeypair {
    pub public: DhPublicKey,
    pub secret: DhSecretKey,
}

impl DhKeypair {
    /// Generate a fresh X25519 keypair using OS entropy.
    pub fn generate() -> Self {
        let mut seed = [0u8; 32];
        OsRng.fill_bytes(&mut seed);
        let secret = StaticSecret::from(seed);
        let public = X25519PublicKey::from(&secret);
        Self {
            public: DhPublicKey(public.to_bytes()),
            secret: DhSecretKey(secret.to_bytes()),
        }
    }

    /// Derive the static X25519 keypair for an Ed25519 identity.
    ///
    /// The secret is the clamped Ed25519 signing scalar (the first half of
    /// `SHA-512(seed)`); the public key is its Montgomery form. Because both
    /// sides can derive this from keys already on the wire, no extra
    /// handshake is needed before the first encrypted datagram.
    pub fn from_ed25519(keypair: &Keypair) -> Self {
        let signing = SigningKey::from_bytes(&keypair.secret.0);
        let secret = StaticSecret::from(signing.to_scalar_bytes());
        let public = X25519PublicKey::from(&secret);
        Self {
            public: DhPublicKey(public.to_bytes()),
            secret: DhSecretKey(secret.to_bytes()),
        }
    }

    /// Derive a shared secret with a peer using X25519 ECDH.
    pub fn diffie_hellman(&self, peer_public: &DhPublicKey) -> [u8; 32] {
        let peer_key = X25519PublicKey::from(peer_public.0);
        let secret = StaticSecret::from(self.secret.to_bytes());
        let shared = secret.diffie_hellman(&peer_key);
        shared.to_bytes()
    }
}

impl Drop for DhKeypair {
    fn drop(&mut self) {
        self.secret.zeroize();
    }
}
