//! Signed discovery beacons: encoding, decoding, and verification.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::crypto::{Keypair, PublicKey, Signature};

use super::constants::{
    BEACON_KIND_ANNOUNCE, BEACON_MAGIC, BEACON_SIGNED_LEN, BEACON_SIZE, BEACON_VERSION,
};
use super::error::BeaconError;

/// Current Unix time in seconds (for [`Beacon::sign`] and the service
/// methods; tests pass explicit timestamps instead).
pub fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The signed portion of a beacon: magic, version, kind, port, timestamp,
/// public key.
fn beacon_prefix(
    listen_port: u16,
    timestamp: u64,
    public_key: &PublicKey,
) -> [u8; BEACON_SIGNED_LEN] {
    let mut prefix = [0u8; BEACON_SIGNED_LEN];
    prefix[0..4].copy_from_slice(&BEACON_MAGIC);
    prefix[4] = BEACON_VERSION;
    prefix[5] = BEACON_KIND_ANNOUNCE;
    prefix[6..8].copy_from_slice(&listen_port.to_be_bytes());
    prefix[8..16].copy_from_slice(&timestamp.to_be_bytes());
    prefix[16..48].copy_from_slice(&public_key.0);
    prefix
}

/// A signed discovery beacon: "node `public_key` listens on `listen_port`
/// at time `timestamp`".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Beacon {
    /// The announcing node's Ed25519 identity.
    pub public_key: PublicKey,
    /// The port the node's main service listens on (beacons themselves
    /// travel on [`DISCOVERY_PORT`]); peers address this port at the
    /// beacon's source IP.
    pub listen_port: u16,
    /// Unix seconds when the beacon was signed.
    pub timestamp: u64,
    /// Ed25519 signature over [`beacon_prefix`] of these fields.
    pub signature: Signature,
}

impl Beacon {
    /// Sign a fresh beacon for `keypair` at `now_unix`.
    pub fn sign(keypair: &Keypair, listen_port: u16, now_unix: u64) -> Self {
        let prefix = beacon_prefix(listen_port, now_unix, &keypair.public);
        Self {
            public_key: keypair.public,
            listen_port,
            timestamp: now_unix,
            signature: keypair.sign(&prefix),
        }
    }

    /// Fixed-size wire bytes (exactly [`BEACON_SIZE`]).
    pub fn encode(&self) -> [u8; BEACON_SIZE] {
        let mut out = [0u8; BEACON_SIZE];
        out[..BEACON_SIGNED_LEN].copy_from_slice(&beacon_prefix(
            self.listen_port,
            self.timestamp,
            &self.public_key,
        ));
        out[BEACON_SIGNED_LEN..].copy_from_slice(&self.signature.0);
        out
    }

    /// Decode wire bytes (size/magic/version only — callers must still
    /// [`Beacon::verify`] for freshness and signature).
    pub fn decode(bytes: &[u8]) -> Result<Self, BeaconError> {
        if bytes.len() != BEACON_SIZE {
            return Err(BeaconError::BadSize { got: bytes.len() });
        }
        if bytes[..4] != BEACON_MAGIC {
            return Err(BeaconError::BadMagic);
        }
        if bytes[4] != BEACON_VERSION {
            return Err(BeaconError::UnsupportedVersion(bytes[4]));
        }
        let listen_port = u16::from_be_bytes(bytes[6..8].try_into().expect("2-byte slice"));
        let timestamp = u64::from_be_bytes(bytes[8..16].try_into().expect("8-byte slice"));
        let mut public_key = [0u8; 32];
        public_key.copy_from_slice(&bytes[16..48]);
        let mut signature = [0u8; 64];
        signature.copy_from_slice(&bytes[48..BEACON_SIZE]);
        Ok(Self {
            public_key: PublicKey(public_key),
            listen_port,
            timestamp,
            signature: Signature(signature),
        })
    }

    /// Validate freshness against `now_unix ± max_age`, then the signature
    /// over the encoded prefix. Freshness is checked first (cheap) so
    /// replayed old datagrams never reach signature verification.
    pub fn verify(&self, now_unix: u64, max_age: Duration) -> Result<(), BeaconError> {
        let max_age_secs = max_age.as_secs();
        if now_unix >= self.timestamp {
            let age_secs = now_unix - self.timestamp;
            if age_secs > max_age_secs {
                return Err(BeaconError::Stale {
                    age_secs,
                    max_age_secs,
                });
            }
        } else {
            let skew_secs = self.timestamp - now_unix;
            if skew_secs > max_age_secs {
                return Err(BeaconError::FromFuture {
                    skew_secs,
                    max_age_secs,
                });
            }
        }
        let prefix = beacon_prefix(self.listen_port, self.timestamp, &self.public_key);
        if !self.signature.verify(&self.public_key, &prefix) {
            return Err(BeaconError::BadSignature);
        }
        Ok(())
    }
}
