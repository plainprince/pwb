//! Ed25519 identity keys and signatures.

use ed25519_dalek::{Signature as Ed25519Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::fmt;

use rand::rngs::OsRng;
use rand::RngCore;

/// 256-bit Identity Public Key (Ed25519).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PublicKey(pub [u8; 32]);

impl PublicKey {
    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{:02x}", b)).collect()
    }
}

impl fmt::Display for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}

/// 256-bit Identity Secret Key (Ed25519).
#[derive(Debug, Clone)]
pub struct SecretKey(pub [u8; 32]);

/// Ed25519 Identity Keypair.
#[derive(Debug, Clone)]
pub struct Keypair {
    pub public: PublicKey,
    pub secret: SecretKey,
}

impl Keypair {
    /// Generate a keypair using Ed25519 and os entropy.
    pub fn generate() -> Self {
        let mut csprng = OsRng;
        let mut seed = [0u8; 32];
        csprng.fill_bytes(&mut seed);
        let signing_key = SigningKey::from_bytes(&seed);
        let verifying_key = signing_key.verifying_key();
        let public = PublicKey(verifying_key.to_bytes());
        let secret = SecretKey(signing_key.to_bytes());
        Self { public, secret }
    }

    /// Sign a message using the keypair's private key.
    pub fn sign(&self, message: &[u8]) -> Signature {
        let signing_key = SigningKey::from_bytes(&self.secret.0);
        let signature = signing_key.sign(message);
        Signature(signature.to_bytes())
    }
}

/// 512-bit Ed25519 Signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Signature(pub [u8; 64]);

impl Serialize for Signature {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Signature {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        let bytes = hex::decode(&s).map_err(serde::de::Error::custom)?;
        if bytes.len() != 64 {
            return Err(serde::de::Error::invalid_length(
                bytes.len(),
                &"64 hex bytes (32-byte Ed25519 signature)",
            ));
        }
        let mut arr = [0u8; 64];
        arr.copy_from_slice(&bytes);
        Ok(Signature(arr))
    }
}

impl Signature {
    /// Verify a signature against the public key and message.
    pub fn verify(&self, public: &PublicKey, message: &[u8]) -> bool {
        let verifying_key = match VerifyingKey::from_bytes(&public.0) {
            Ok(vk) => vk,
            Err(_) => return false,
        };
        verifying_key
            .verify(message, &Ed25519Signature::from_bytes(&self.0))
            .is_ok()
    }

    pub fn to_bytes(&self) -> [u8; 64] {
        self.0
    }

    pub fn from_bytes(bytes: [u8; 64]) -> Self {
        Self(bytes)
    }

    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{:02x}", b)).collect()
    }
}
