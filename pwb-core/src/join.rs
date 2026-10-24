//! Well-known join descriptors (`pwb-core::join`).
//!
//! A fresh client needs two pieces of information before it can join a public
//! swarm: at least one dialable seed (`host:port` plus its pinned Ed25519 key)
//! and, when the deployment pins funding attestors, the attestor keys. This
//! module defines a small, signed JSON document that carries both, so a client
//! can obtain them from a stable HTTPS URL instead of requiring an
//! out-of-band connection link.
//!
//! The descriptor is *not* a trust root for content or claims. It only names
//! rendezvous points and attestor keys; the seed remains an availability
//! dependency, and every message received through it is still authenticated by
//! the normal envelope/onion checks. A deployment can additionally sign the
//! descriptor so mirrors and cached copies cannot rewrite the seed list, and
//! clients can trust-on-first-use the signer key (see `pwb_node::join`).
//!
//! Wire shape:
//!
//! ```json
//! {
//!   "version": 1,
//!   "generated_at": 1761264000,
//!   "expires_at": null,
//!   "seeds": [{"address": "seed.example.com:7921", "public_key": "<hex>"}],
//!   "trust_anchors": ["<hex>"],
//!   "signer": "<hex>",
//!   "signature": "<hex>"
//! }
//! ```
//!
//! `signature` covers a canonical byte encoding of every field except
//! `signature` itself, so JSON formatting, key order, and mirroring do not
//! affect verification.

use serde::{Deserialize, Serialize};

use crate::crypto::{Keypair, PublicKey, Signature};

/// Domain-separation prefix for join-descriptor signatures.
pub const JOIN_SIGN_PREFIX: &[u8] = b"PWBJOIN1";

/// Maximum seeds accepted from one descriptor.
pub const MAX_JOIN_SEEDS: usize = 64;
/// Maximum pinned attestors accepted from one descriptor.
pub const MAX_JOIN_ANCHORS: usize = 64;
/// Longest seed address accepted (`host:port` or `[v6]:port`).
pub const MAX_SEED_ADDRESS_LEN: usize = 255;

/// One rendezvous seed published in a join descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinSeed {
    /// `host:port`; the host may be a DNS name.
    pub address: String,
    /// The seed's pinned Ed25519 identity key.
    pub public_key: PublicKey,
}

/// A parsed and structurally validated join descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinDescriptor {
    /// Wire version; only [`JOIN_VERSION`] is accepted.
    pub version: u8,
    /// Unix seconds when the descriptor was produced.
    pub generated_at: u64,
    /// Optional hard expiry. `None` means the operator rotates it explicitly.
    pub expires_at: Option<u64>,
    /// Rendezvous seeds, in operator preference order.
    pub seeds: Vec<JoinSeed>,
    /// Attestor keys that may fund domain claims.
    pub trust_anchors: Vec<PublicKey>,
    /// Key that signed the canonical descriptor, when signed.
    pub signer: Option<PublicKey>,
    /// Signature over [`JoinDescriptor::signing_bytes`].
    pub signature: Option<Signature>,
}

/// Current join-descriptor wire version.
pub const JOIN_VERSION: u8 = 1;

/// Why a join descriptor was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinError {
    /// The document was not valid JSON or had the wrong field types.
    Malformed(String),
    /// The descriptor version is not understood.
    UnsupportedVersion(u8),
    /// The descriptor has too many entries.
    TooManyEntries,
    /// A seed address is empty, too long, or not a `host:port` pair.
    BadSeedAddress(String),
    /// A hexadecimal key or signature was not the expected length.
    BadHex(String),
    /// The expiry precedes `generated_at` or the descriptor has expired.
    Expired,
    /// A signature or signer field was present without its counterpart.
    IncompleteSignature,
    /// The signature did not verify against the declared signer.
    BadSignature,
}

impl std::fmt::Display for JoinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JoinError::Malformed(e) => write!(f, "malformed join descriptor: {e}"),
            JoinError::UnsupportedVersion(v) => write!(f, "unsupported join version {v}"),
            JoinError::TooManyEntries => write!(f, "join descriptor has too many entries"),
            JoinError::BadSeedAddress(a) => write!(f, "invalid seed address '{a}'"),
            JoinError::BadHex(field) => write!(f, "invalid hex in {field}"),
            JoinError::Expired => write!(f, "join descriptor has expired"),
            JoinError::IncompleteSignature => {
                write!(f, "join descriptor has an incomplete signature")
            }
            JoinError::BadSignature => write!(f, "join descriptor signature did not verify"),
        }
    }
}

impl std::error::Error for JoinError {}

/// JSON representation. Public keys and signatures use hex so descriptors are
/// stable across serializers and readable by operators.
#[derive(Debug, Serialize, Deserialize)]
struct JoinFile {
    version: u8,
    generated_at: u64,
    #[serde(default)]
    expires_at: Option<u64>,
    #[serde(default)]
    seeds: Vec<JoinSeedFile>,
    #[serde(default)]
    trust_anchors: Vec<String>,
    #[serde(default)]
    signer: Option<String>,
    #[serde(default)]
    signature: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct JoinSeedFile {
    address: String,
    public_key: String,
}

fn parse_key(hex: &str, field: &str) -> Result<PublicKey, JoinError> {
    let bytes = hex::decode(hex.trim()).map_err(|_| JoinError::BadHex(field.to_string()))?;
    if bytes.len() != 32 {
        return Err(JoinError::BadHex(field.to_string()));
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&bytes);
    Ok(PublicKey(key))
}

fn parse_signature(hex: &str) -> Result<Signature, JoinError> {
    let bytes = hex::decode(hex.trim()).map_err(|_| JoinError::BadHex("signature".to_string()))?;
    if bytes.len() != 64 {
        return Err(JoinError::BadHex("signature".to_string()));
    }
    let mut sig = [0u8; 64];
    sig.copy_from_slice(&bytes);
    Ok(Signature(sig))
}

fn push_len_prefixed(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
    out.extend_from_slice(bytes);
}

impl JoinDescriptor {
    /// Parse and structurally validate a descriptor. This does not check the
    /// signature or expiry; call [`Self::verify_signature`] and
    /// [`Self::ensure_current`] for those.
    pub fn parse(bytes: &[u8]) -> Result<Self, JoinError> {
        let file: JoinFile =
            serde_json::from_slice(bytes).map_err(|e| JoinError::Malformed(e.to_string()))?;
        Self::from_file(file)
    }

    fn from_file(file: JoinFile) -> Result<Self, JoinError> {
        if file.version != JOIN_VERSION {
            return Err(JoinError::UnsupportedVersion(file.version));
        }
        if file.seeds.len() > MAX_JOIN_SEEDS || file.trust_anchors.len() > MAX_JOIN_ANCHORS {
            return Err(JoinError::TooManyEntries);
        }
        if let Some(expires) = file.expires_at {
            if expires <= file.generated_at {
                return Err(JoinError::Expired);
            }
        }

        let mut seeds = Vec::with_capacity(file.seeds.len());
        for seed in file.seeds {
            let address = seed.address.trim().to_string();
            if address.is_empty()
                || address.len() > MAX_SEED_ADDRESS_LEN
                || !address.contains(':')
                || address.chars().any(char::is_whitespace)
                || address.contains('/')
            {
                return Err(JoinError::BadSeedAddress(address));
            }
            seeds.push(JoinSeed {
                address,
                public_key: parse_key(&seed.public_key, "seeds[].public_key")?,
            });
        }

        let mut trust_anchors = Vec::with_capacity(file.trust_anchors.len());
        for anchor in file.trust_anchors {
            trust_anchors.push(parse_key(&anchor, "trust_anchors[]")?);
        }
        trust_anchors.sort_unstable_by_key(|key| key.0);
        trust_anchors.dedup();

        let signer = match &file.signer {
            Some(signer) => Some(parse_key(signer, "signer")?),
            None => None,
        };
        let signature = match &file.signature {
            Some(signature) => Some(parse_signature(signature)?),
            None => None,
        };
        if signer.is_some() != signature.is_some() {
            return Err(JoinError::IncompleteSignature);
        }

        let descriptor = Self {
            version: file.version,
            generated_at: file.generated_at,
            expires_at: file.expires_at,
            seeds,
            trust_anchors,
            signer,
            signature,
        };
        descriptor.validate_seed_keys()?;
        Ok(descriptor)
    }

    fn validate_seed_keys(&self) -> Result<(), JoinError> {
        // A zero key cannot be a real Ed25519 identity; reject it early rather
        // than repeatedly failing the liveness probe.
        for seed in &self.seeds {
            if seed.public_key.0 == [0u8; 32] {
                return Err(JoinError::BadHex("seeds[].public_key".to_string()));
            }
        }
        Ok(())
    }

    /// Canonical bytes covered by `signature`. Every field except the signature
    /// itself is length-prefixed, so adding a seed cannot shift the meaning of
    /// another field.
    pub fn signing_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(64 + self.seeds.len() * 80);
        out.extend_from_slice(JOIN_SIGN_PREFIX);
        out.push(self.version);
        out.extend_from_slice(&self.generated_at.to_le_bytes());
        match self.expires_at {
            Some(expires) => {
                out.push(1);
                out.extend_from_slice(&expires.to_le_bytes());
            }
            None => out.push(0),
        }
        out.extend_from_slice(&(self.seeds.len() as u16).to_le_bytes());
        for seed in &self.seeds {
            push_len_prefixed(&mut out, seed.address.as_bytes());
            out.extend_from_slice(&seed.public_key.0);
        }
        out.extend_from_slice(&(self.trust_anchors.len() as u16).to_le_bytes());
        for anchor in &self.trust_anchors {
            out.extend_from_slice(&anchor.0);
        }
        out.extend_from_slice(&self.signer.map(|key| key.0).unwrap_or([0u8; 32]));
        out
    }

    /// Sign the canonical fields and store the signer key.
    pub fn sign(&mut self, keypair: &Keypair) {
        self.signer = Some(keypair.public);
        self.signature = Some(keypair.sign(&self.signing_bytes()));
    }

    /// Verify the descriptor signature. Unsigned descriptors are accepted here
    /// because the fetch layer decides whether a signature is required (for
    /// example, a first-use pin).
    pub fn verify_signature(&self) -> Result<(), JoinError> {
        match (self.signer, self.signature) {
            (None, None) => Ok(()),
            (Some(signer), Some(signature)) => {
                if signature.verify(&signer, &self.signing_bytes()) {
                    Ok(())
                } else {
                    Err(JoinError::BadSignature)
                }
            }
            _ => Err(JoinError::IncompleteSignature),
        }
    }

    /// Reject a descriptor whose optional expiry has passed.
    pub fn ensure_current(&self, now: u64) -> Result<(), JoinError> {
        if self.expires_at.is_some_and(|expires| now >= expires) {
            return Err(JoinError::Expired);
        }
        Ok(())
    }

    /// Serialize the descriptor as pretty JSON, including its signature.
    pub fn to_json(&self) -> Result<Vec<u8>, JoinError> {
        let file = JoinFile {
            version: self.version,
            generated_at: self.generated_at,
            expires_at: self.expires_at,
            seeds: self
                .seeds
                .iter()
                .map(|seed| JoinSeedFile {
                    address: seed.address.clone(),
                    public_key: seed.public_key.to_hex(),
                })
                .collect(),
            trust_anchors: self.trust_anchors.iter().map(PublicKey::to_hex).collect(),
            signer: self.signer.map(|key| key.to_hex()),
            signature: self.signature.map(|sig| sig.to_hex()),
        };
        serde_json::to_vec_pretty(&file).map_err(|e| JoinError::Malformed(e.to_string()))
    }

    /// Merge another descriptor's seeds and anchors, deduplicating by key and
    /// address. Used when a client has several join sources.
    pub fn merge(&mut self, other: &JoinDescriptor) {
        for seed in &other.seeds {
            if !self
                .seeds
                .iter()
                .any(|have| have.address == seed.address && have.public_key == seed.public_key)
            {
                self.seeds.push(seed.clone());
            }
        }
        self.trust_anchors
            .extend(other.trust_anchors.iter().copied());
        self.trust_anchors.sort_unstable_by_key(|key| key.0);
        self.trust_anchors.dedup();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::Keypair;

    fn descriptor() -> JoinDescriptor {
        JoinDescriptor {
            version: JOIN_VERSION,
            generated_at: 1_700_000_000,
            expires_at: Some(1_900_000_000),
            seeds: vec![
                JoinSeed {
                    address: "seed.example.com:7921".to_string(),
                    public_key: Keypair::generate().public,
                },
                JoinSeed {
                    address: "[2001:db8::1]:7921".to_string(),
                    public_key: Keypair::generate().public,
                },
            ],
            trust_anchors: vec![Keypair::generate().public],
            signer: None,
            signature: None,
        }
    }

    #[test]
    fn round_trips_and_verifies_a_signed_descriptor() {
        let mut descriptor = descriptor();
        let signer = Keypair::generate();
        descriptor.sign(&signer);
        descriptor.verify_signature().expect("signature verifies");

        let json = descriptor.to_json().unwrap();
        let parsed = JoinDescriptor::parse(&json).expect("parses");
        assert_eq!(parsed, descriptor);
        parsed
            .verify_signature()
            .expect("parsed signature verifies");
    }

    #[test]
    fn rejects_a_tampered_seed() {
        let mut descriptor = descriptor();
        descriptor.sign(&Keypair::generate());
        let mut json: serde_json::Value =
            serde_json::from_slice(&descriptor.to_json().unwrap()).expect("descriptor is JSON");
        json["seeds"][0]["address"] = serde_json::json!("evil.example:7921");
        let parsed = JoinDescriptor::parse(&serde_json::to_vec(&json).unwrap()).unwrap();
        assert_eq!(
            parsed.verify_signature(),
            Err(JoinError::BadSignature),
            "changing a signed field must invalidate the signature"
        );
    }

    #[test]
    fn rejects_incomplete_signatures_and_bad_hex() {
        let mut descriptor = descriptor();
        descriptor.signer = Some(Keypair::generate().public);
        assert_eq!(
            descriptor.verify_signature(),
            Err(JoinError::IncompleteSignature)
        );

        let json = br#"{
            "version": 1,
            "generated_at": 1,
            "seeds": [{"address": "seed.example:7921", "public_key": "nothex"}]
        }"#;
        assert!(matches!(
            JoinDescriptor::parse(json),
            Err(JoinError::BadHex(_))
        ));
    }

    #[test]
    fn rejects_bad_addresses_and_versions() {
        let mut descriptor = descriptor();
        descriptor.seeds[0].address = "no-port".to_string();
        let json = descriptor.to_json().unwrap();
        assert!(matches!(
            JoinDescriptor::parse(&json),
            Err(JoinError::BadSeedAddress(_))
        ));

        let mut json: serde_json::Value =
            serde_json::from_slice(&descriptor.to_json().unwrap()).expect("descriptor is JSON");
        json["version"] = serde_json::json!(99);
        assert!(matches!(
            JoinDescriptor::parse(&serde_json::to_vec(&json).unwrap()),
            Err(JoinError::UnsupportedVersion(99))
        ));
    }

    #[test]
    fn enforces_expiry_and_deduplicates_anchors() {
        let descriptor = descriptor();
        assert_eq!(descriptor.ensure_current(1_800_000_000), Ok(()));
        assert_eq!(
            descriptor.ensure_current(2_000_000_000),
            Err(JoinError::Expired)
        );

        let mut left = descriptor.clone();
        let right = descriptor;
        left.merge(&right);
        assert_eq!(left.seeds.len(), 2, "duplicate seeds collapse");
        assert_eq!(left.trust_anchors.len(), 1, "duplicate anchors collapse");
    }
}
