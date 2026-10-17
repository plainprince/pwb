//! X25519 Diffie-Hellman key agreement.

use std::fmt;

use rand::rngs::OsRng;
use rand::RngCore;

use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// X25519 Diffie-Hellman public key (32 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DhPublicKey(pub [u8; 32]);

impl DhPublicKey {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
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
