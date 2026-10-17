//! Issuer-side blind Schnorr signing keys and one-time nonces.

use curve25519_dalek::constants::RISTRETTO_BASEPOINT_TABLE;
use curve25519_dalek::ristretto::{CompressedRistretto, RistrettoPoint};
use curve25519_dalek::scalar::Scalar;

use rand::rngs::OsRng;
use rand::RngCore;

use zeroize::Zeroize;

use super::voucher::{
    scalar_from_parts, BlindVoucherPartialSignature, ScalarBytes, VoucherBlindRequest,
    BLIND_VOUCHER_DOMAIN,
};

/// Ristretto public key of a blind voucher issuer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlindVoucherPublicKey {
    point: CompressedRistretto,
}

impl BlindVoucherPublicKey {
    pub fn to_bytes(&self) -> [u8; 32] {
        self.point.to_bytes()
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Option<Self> {
        CompressedRistretto(bytes).decompress().map(|point| Self {
            point: point.compress(),
        })
    }

    pub(super) fn as_point(&self) -> Option<RistrettoPoint> {
        self.point.decompress()
    }
}

/// One-time issuer nonce for a single blinded signing request.
pub struct BlindVoucherNonce {
    pub(super) point: RistrettoPoint,
    nonce: ScalarBytes,
}

impl BlindVoucherNonce {
    /// Issuer commitment sent to the requester.
    pub fn to_bytes(&self) -> [u8; 32] {
        self.point.compress().to_bytes()
    }

    /// Reconstruct the public commitment from its 32-byte wire encoding.
    ///
    /// The requester side of the protocol only needs the point (to build the
    /// blinded challenge in [`BlindVoucher::blind_request`]); the secret
    /// scalar stays with the issuer. The reconstructed nonce carries a
    /// placeholder scalar and must never be passed to
    /// [`BlindVoucherIssuer::sign_blind`] — only the issuer's own nonces from
    /// [`BlindVoucherIssuer::signing_nonce`] may sign.
    ///
    /// [`BlindVoucher::blind_request`]: super::voucher::BlindVoucher::blind_request
    pub fn from_point(point: [u8; 32]) -> Option<Self> {
        let decompressed = CompressedRistretto(point).decompress()?;
        Some(BlindVoucherNonce {
            point: decompressed,
            nonce: ScalarBytes([0u8; 32]),
        })
    }
}

/// Issuer keypair producing Chaum-style blind Schnorr signatures.
///
/// # Denomination enforcement
///
/// A blind signature hides the message, so an issuer cannot inspect the amount
/// it is signing. Instead, authorization is enforced structurally: derive one
/// issuer key per denomination with [`BlindVoucherIssuer::from_denomination_seed`],
/// and [`BlindVoucherIssuer::sign_blind`] then refuses any amount outside that
/// key's denomination. A voucher signed by the `100` key can only ever verify
/// against the `100` public key, so credit volume cannot be forged.
///
/// # Nonce safety
///
/// Every nonce from [`BlindVoucherIssuer::signing_nonce`] or
/// [`BlindVoucherIssuer::nonce_pool`] is single-use and consumed by value in
/// [`BlindVoucherIssuer::sign_blind`]. Signing two different requests with one
/// nonce would reveal the issuer secret.
pub struct BlindVoucherIssuer {
    secret: ScalarBytes,
    public: RistrettoPoint,
    denomination: Option<u64>,
}

impl BlindVoucherIssuer {
    /// Generate an issuer key that accepts any amount.
    pub fn generate() -> Self {
        let mut seed = [0u8; 32];
        OsRng.fill_bytes(&mut seed);
        Self::from_seed(seed)
    }

    /// Build an issuer key that accepts any amount from a seed.
    pub fn from_seed(seed: [u8; 32]) -> Self {
        Self::build(seed, None)
    }

    /// Derive the issuer key for a single denomination from a master seed.
    ///
    /// Distinct denominations yield independent keys with no algebraic
    /// relation, so compromising one denomination does not forge another.
    pub fn from_denomination_seed(master_seed: &[u8; 32], amount: u64) -> Self {
        let mut wide = [0u8; 64];
        let mut reader = blake3::Hasher::new()
            .update(BLIND_VOUCHER_DOMAIN)
            .update(b"denomination")
            .update(master_seed)
            .update(&amount.to_le_bytes())
            .finalize_xof();
        reader.fill(&mut wide);
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&wide[0..32]);
        Self::build(seed, Some(amount))
    }

    fn build(seed: [u8; 32], denomination: Option<u64>) -> Self {
        let secret = Scalar::from_bytes_mod_order(seed);
        let public = &secret * RISTRETTO_BASEPOINT_TABLE;
        Self {
            secret: ScalarBytes(secret.to_bytes()),
            public,
            denomination,
        }
    }

    /// Denomination this key is bound to, or `None` when unbound.
    pub fn denomination(&self) -> Option<u64> {
        self.denomination
    }

    /// Whether this key will authorize the given amount.
    pub fn accepts_amount(&self, amount: u64) -> bool {
        match self.denomination {
            Some(denomination) => denomination == amount,
            None => true,
        }
    }

    pub fn public_key(&self) -> BlindVoucherPublicKey {
        BlindVoucherPublicKey {
            point: self.public.compress(),
        }
    }

    /// Round 1: commit to a fresh one-time nonce.
    pub fn signing_nonce(&self) -> BlindVoucherNonce {
        let mut seed = [0u8; 32];
        OsRng.fill_bytes(&mut seed);
        self.signing_nonce_from_seed(seed)
    }

    /// Pre-generate a pool of one-time nonces.
    ///
    /// An issuer can publish these up front, which turns issuance into a single
    /// online round trip: a requester picks an unused nonce and sends only its
    /// blinded challenge. Nonces must never be handed out twice or used twice.
    pub fn nonce_pool(&self, count: usize) -> Vec<BlindVoucherNonce> {
        (0..count).map(|_| self.signing_nonce()).collect()
    }

    fn signing_nonce_from_seed(&self, seed: [u8; 32]) -> BlindVoucherNonce {
        let nonce = scalar_from_parts(b"issuer-nonce", &seed);
        let scalar = Scalar::from_bytes_mod_order(nonce.0);
        BlindVoucherNonce {
            point: &scalar * RISTRETTO_BASEPOINT_TABLE,
            nonce,
        }
    }
}

impl Drop for BlindVoucherIssuer {
    fn drop(&mut self) {
        self.secret.zeroize();
    }
}

impl BlindVoucherIssuer {
    /// Round 2: sign the blinded challenge with the one-time nonce.
    ///
    /// Consuming `nonce` by value enforces single use, which is the property
    /// blind Schnorr signatures rely on for unforgeability. Returns `None`
    /// when the request's amount falls outside this key's denomination.
    pub fn sign_blind(
        &self,
        nonce: BlindVoucherNonce,
        request: &VoucherBlindRequest,
    ) -> Option<BlindVoucherPartialSignature> {
        if !self.accepts_amount(request.amount) {
            return None;
        }
        let secret = Scalar::from_bytes_mod_order(self.secret.0);
        let nonce_scalar = Scalar::from_bytes_mod_order(nonce.nonce.0);
        let challenge = Scalar::from_bytes_mod_order(request.blinded_challenge);
        Some(BlindVoucherPartialSignature {
            response: ScalarBytes((nonce_scalar + challenge * secret).to_bytes()),
        })
    }
}
