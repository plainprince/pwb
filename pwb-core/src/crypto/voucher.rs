//! Bearer blind credit vouchers: requester-side issuance, ownership proofs,
//! and public verification (issuer-side keys live in [`super::issuer`]).

use curve25519_dalek::constants::RISTRETTO_BASEPOINT_TABLE;
use curve25519_dalek::ristretto::{CompressedRistretto, RistrettoPoint};
use curve25519_dalek::scalar::Scalar;
use std::fmt;

use rand::rngs::OsRng;
use rand::RngCore;

use zeroize::{Zeroize, ZeroizeOnDrop};

use super::issuer::{BlindVoucherNonce, BlindVoucherPublicKey};

/// Bearer secret that defines a blind credit voucher's hidden token.
///
/// The token point is derived from this secret, so the holder can prove
/// ownership without revealing it.
#[derive(Clone, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub struct VoucherSecret(pub [u8; 32]);

impl VoucherSecret {
    pub fn random() -> Self {
        let mut bytes = [0u8; 32];
        OsRng.fill_bytes(&mut bytes);
        Self(bytes)
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn to_bytes(&self) -> [u8; 32] {
        self.0
    }
}

impl fmt::Debug for VoucherSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("VoucherSecret(redacted)")
    }
}

/// One-time blinding scalars a requester applies before an issuer signs.
///
/// Reusing blinding factors across issuances would let an issuer link the
/// resulting vouchers, so they are generated per request.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct VoucherBlinding {
    alpha: ScalarBytes,
    beta: ScalarBytes,
}

impl VoucherBlinding {
    pub fn random() -> Self {
        let mut seed = [0u8; 32];
        OsRng.fill_bytes(&mut seed);
        Self::from_seed(seed)
    }

    pub fn from_seed(seed: [u8; 32]) -> Self {
        Self {
            alpha: scalar_from_parts(b"blinding-alpha", &seed),
            beta: scalar_from_parts(b"blinding-beta", &seed),
        }
    }
}

impl fmt::Debug for VoucherBlinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("VoucherBlinding(redacted)")
    }
}

/// 32-byte scalar wrapper with redacted debug output and zeroization.
#[derive(Clone, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub(super) struct ScalarBytes(pub [u8; 32]);

impl fmt::Debug for ScalarBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ScalarBytes(redacted)")
    }
}

/// Domain separation for every blind voucher derivation.
pub(super) const BLIND_VOUCHER_DOMAIN: &[u8] = b"pwb-blind-voucher-schnorr-v1";

pub(super) fn scalar_from_parts(domain_part: &[u8], seed: &[u8; 32]) -> ScalarBytes {
    let mut wide = [0u8; 64];
    let mut reader = blake3::Hasher::new()
        .update(BLIND_VOUCHER_DOMAIN)
        .update(domain_part)
        .update(seed)
        .finalize_xof();
    reader.fill(&mut wide);
    ScalarBytes(Scalar::from_bytes_mod_order_wide(&wide).to_bytes())
}

/// Derive the token scalar hiding the holder's bearer secret.
fn voucher_token_scalar(secret: &[u8; 32]) -> Scalar {
    let mut wide = [0u8; 64];
    let mut reader = blake3::Hasher::new()
        .update(BLIND_VOUCHER_DOMAIN)
        .update(b"token-scalar")
        .update(secret)
        .finalize_xof();
    reader.fill(&mut wide);
    Scalar::from_bytes_mod_order_wide(&wide)
}

/// Hidden token point `M = token_scalar * G`.
///
/// Deriving the token as a scalar multiple (rather than an arbitrary
/// hash-to-curve point) is what makes a zero-knowledge proof of ownership
/// possible: the holder can prove knowledge of the discrete log without
/// revealing it.
fn voucher_token(secret: &[u8; 32]) -> RistrettoPoint {
    &voucher_token_scalar(secret) * RISTRETTO_BASEPOINT_TABLE
}

/// Fiat-Shamir challenge for the ownership proof.
fn ownership_challenge(amount: u64, token: &[u8; 32], commitment: &RistrettoPoint) -> Scalar {
    let mut wide = [0u8; 64];
    let mut reader = blake3::Hasher::new()
        .update(BLIND_VOUCHER_DOMAIN)
        .update(b"ownership")
        .update(&amount.to_le_bytes())
        .update(token)
        .update(commitment.compress().as_bytes())
        .finalize_xof();
    reader.fill(&mut wide);
    Scalar::from_bytes_mod_order_wide(&wide)
}

/// Zero-knowledge proof that the prover knows the voucher's token scalar.
///
/// This is a Schnorr proof of knowledge with Fiat-Shamir: it convinces a
/// verifier that the prover controls the token behind a voucher without
/// revealing the bearer secret or any information about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoucherOwnershipProof {
    pub commitment: [u8; 32],
    pub response: [u8; 32],
}

impl VoucherOwnershipProof {
    /// Serialize the proof losslessly as 64 raw bytes.
    pub fn to_bytes(&self) -> [u8; 64] {
        let mut out = [0u8; 64];
        out[0..32].copy_from_slice(&self.commitment);
        out[32..64].copy_from_slice(&self.response);
        out
    }

    /// Deserialize a proof from its 64-byte encoding.
    pub fn from_bytes(bytes: &[u8; 64]) -> Self {
        let mut commitment = [0u8; 32];
        let mut response = [0u8; 32];
        commitment.copy_from_slice(&bytes[0..32]);
        response.copy_from_slice(&bytes[32..64]);
        Self {
            commitment,
            response,
        }
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.to_bytes())
    }
}

/// Fiat-Shamir challenge binding issuer key, commitment, token and amount.
fn blind_voucher_challenge(
    amount: u64,
    issuer_public: &RistrettoPoint,
    commitment: &RistrettoPoint,
    token: &RistrettoPoint,
) -> Scalar {
    let mut wide = [0u8; 64];
    let mut reader = blake3::Hasher::new()
        .update(BLIND_VOUCHER_DOMAIN)
        .update(b"challenge")
        .update(&amount.to_le_bytes())
        .update(issuer_public.compress().as_bytes())
        .update(commitment.compress().as_bytes())
        .update(token.compress().as_bytes())
        .finalize_xof();
    reader.fill(&mut wide);
    Scalar::from_bytes_mod_order_wide(&wide)
}

/// Blinded signing request an issuer signs without learning the token.
///
/// The issuer cannot see the token or the amount inside a blind signature, so
/// the amount is authorized structurally instead: an issuer derived with
/// [`BlindVoucherIssuer::from_denomination_seed`] only signs its own
/// denomination, and the amount is bound into the final challenge so a voucher
/// verifies for exactly the amount it was issued for.
///
/// [`BlindVoucherIssuer::from_denomination_seed`]: super::issuer::BlindVoucherIssuer::from_denomination_seed
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoucherBlindRequest {
    pub amount: u64,
    pub blinded_challenge: [u8; 32],
}

/// Requester-held state needed to unblind an issuer's response.
///
/// Holding this is what lets the requester remove the blinding factor, so it
/// must stay private until the voucher is finished.
pub struct VoucherBlindState {
    amount: u64,
    token: RistrettoPoint,
    challenge: ScalarBytes,
    alpha: ScalarBytes,
}

impl fmt::Debug for VoucherBlindState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("VoucherBlindState(redacted)")
    }
}

/// Issuer response, still blinded by the requester's challenge factor.
#[derive(Clone, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub struct BlindVoucherPartialSignature {
    pub(super) response: ScalarBytes,
}

impl BlindVoucherPartialSignature {
    pub fn to_bytes(&self) -> [u8; 32] {
        self.response.0
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Option<Self> {
        let canonical: Option<Scalar> = Option::from(Scalar::from_canonical_bytes(bytes));
        canonical.map(|_| Self {
            response: ScalarBytes(bytes),
        })
    }
}

impl fmt::Debug for BlindVoucherPartialSignature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BlindVoucherPartialSignature(redacted)")
    }
}

/// Unlinkable blind credit voucher authorizing `amount` credits.
///
/// The voucher is a plain Schnorr signature over a hidden token point, so it
/// is verifiable by anyone holding only the issuer's public key. Redemption
/// reveals neither the bearer secret nor which issuance produced it; the
/// public token acts as the spend identifier for double-spend tracking.
///
/// # Guarantees
///
/// * **Public verification** — [`BlindVoucher::verify`] needs only the issuer's
///   public key, so peer attestors can validate a voucher independently.
/// * **Zero-knowledge ownership** — [`BlindVoucher::prove_ownership`] proves the
///   holder knows the token scalar via a Schnorr proof of knowledge, so a
///   redeemer never reveals the bearer secret.
/// * **Unlinkability** — the issuer only ever sees blinded values, so it cannot
///   connect a redemption to the issuance request that produced it.
/// * **Denomination binding** — the `challenge` binds `amount`, the issuer key,
///   the commitment and the token, so no field can be altered. Combined with
///   per-denomination issuer keys, credit volume cannot be forged.
/// * **Replay rejection** — [`VoucherSpendLog`] records spent token identifiers
///   and returns [`VoucherSpendError::AlreadySpent`] on repeat presentation.
///
/// Vouchers are bearer instruments: whoever holds the bytes may present them,
/// which is why the spend log, not the voucher itself, decides double spends.
///
/// [`VoucherSpendLog`]: super::spend::VoucherSpendLog
/// [`VoucherSpendError::AlreadySpent`]: super::spend::VoucherSpendError::AlreadySpent
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlindVoucher {
    pub amount: u64,
    pub token: [u8; 32],
    pub challenge: [u8; 32],
    pub response: [u8; 32],
}

impl BlindVoucher {
    /// Requester step 1: build the blinded request for an issuer nonce.
    ///
    /// `R = nonce + alpha*G + beta*X` keeps the real commitment hidden from
    /// the issuer, and `c' = c + beta` keeps the challenge blinded.
    pub fn blind_request(
        amount: u64,
        secret: &VoucherSecret,
        blinding: &VoucherBlinding,
        issuer_public: &BlindVoucherPublicKey,
        nonce: &BlindVoucherNonce,
    ) -> Option<(VoucherBlindState, VoucherBlindRequest)> {
        let issuer_point = issuer_public.as_point()?;
        let token = voucher_token(&secret.0);
        let alpha = Scalar::from_bytes_mod_order(blinding.alpha.0);
        let beta = Scalar::from_bytes_mod_order(blinding.beta.0);

        let commitment = nonce.point + &alpha * RISTRETTO_BASEPOINT_TABLE + beta * issuer_point;
        let challenge = blind_voucher_challenge(amount, &issuer_point, &commitment, &token);

        Some((
            VoucherBlindState {
                amount,
                token,
                challenge: ScalarBytes(challenge.to_bytes()),
                alpha: blinding.alpha.clone(),
            },
            VoucherBlindRequest {
                amount,
                blinded_challenge: (challenge + beta).to_bytes(),
            },
        ))
    }

    /// Requester step 2: unblind the issuer response into a voucher.
    ///
    /// `s = s' + alpha` removes the blinding so the result verifies against
    /// the issuer's public key while remaining unlinkable to `s'`.
    pub fn unblind(
        state: VoucherBlindState,
        partial: &BlindVoucherPartialSignature,
    ) -> Option<Self> {
        let alpha = Scalar::from_bytes_mod_order(state.alpha.0);
        let challenge = Scalar::from_bytes_mod_order(state.challenge.0);
        let response = Scalar::from_bytes_mod_order(partial.response.0) + alpha;

        Some(Self {
            amount: state.amount,
            token: state.token.compress().to_bytes(),
            challenge: challenge.to_bytes(),
            response: response.to_bytes(),
        })
    }

    /// Publicly verify the voucher with only the issuer's public key.
    pub fn verify(&self, issuer_public: &BlindVoucherPublicKey) -> bool {
        let Some(issuer_point) = issuer_public.as_point() else {
            return false;
        };
        let Some(token) = CompressedRistretto(self.token).decompress() else {
            return false;
        };
        let Some(challenge): Option<Scalar> =
            Option::from(Scalar::from_canonical_bytes(self.challenge))
        else {
            return false;
        };
        let Some(response): Option<Scalar> =
            Option::from(Scalar::from_canonical_bytes(self.response))
        else {
            return false;
        };

        // Recover the commitment the issuer signed: R = s*G - c*X.
        let commitment = &response * RISTRETTO_BASEPOINT_TABLE - challenge * issuer_point;
        blind_voucher_challenge(self.amount, &issuer_point, &commitment, &token) == challenge
    }

    /// Local check that the supplied secret opens this voucher's token.
    ///
    /// The secret never leaves the caller, so this leaks nothing, but the
    /// caller must hold the secret. To convince a third party without revealing
    /// it, use [`BlindVoucher::prove_ownership`] instead.
    pub fn verify_owner(&self, secret: &VoucherSecret) -> bool {
        voucher_token(&secret.0).compress().to_bytes() == self.token
    }

    /// Produce a zero-knowledge proof of ownership for a third party.
    ///
    /// Returns `None` when the supplied secret does not open this voucher. The
    /// proof is bound to the voucher's amount and token, so it cannot be
    /// replayed against a different voucher.
    pub fn prove_ownership(&self, secret: &VoucherSecret) -> Option<VoucherOwnershipProof> {
        let token_scalar = voucher_token_scalar(&secret.0);
        if (&token_scalar * RISTRETTO_BASEPOINT_TABLE)
            .compress()
            .to_bytes()
            != self.token
        {
            return None;
        }

        let mut seed = [0u8; 32];
        OsRng.fill_bytes(&mut seed);
        let nonce = scalar_from_parts(b"ownership-nonce", &seed);
        let nonce_scalar = Scalar::from_bytes_mod_order(nonce.0);
        let commitment = &nonce_scalar * RISTRETTO_BASEPOINT_TABLE;
        let challenge = ownership_challenge(self.amount, &self.token, &commitment);
        let response = nonce_scalar + challenge * token_scalar;

        Some(VoucherOwnershipProof {
            commitment: commitment.compress().to_bytes(),
            response: response.to_bytes(),
        })
    }

    /// Verify a zero-knowledge ownership proof without any secret.
    ///
    /// Checks `s*G == R + e*M`, so the verifier learns only that the prover
    /// knows the token scalar. The bearer secret stays private.
    pub fn verify_ownership(&self, proof: &VoucherOwnershipProof) -> bool {
        let Some(token_point) = self.token_point() else {
            return false;
        };
        let Some(commitment) = CompressedRistretto(proof.commitment).decompress() else {
            return false;
        };
        let Some(response): Option<Scalar> =
            Option::from(Scalar::from_canonical_bytes(proof.response))
        else {
            return false;
        };

        let challenge = ownership_challenge(self.amount, &self.token, &commitment);
        &response * RISTRETTO_BASEPOINT_TABLE == commitment + challenge * token_point
    }

    /// Token bytes identifying this voucher, used for double-spend tracking.
    pub fn spend_id(&self) -> [u8; 32] {
        self.token
    }

    /// Decoded hidden token point, if the encoding is valid.
    pub fn token_point(&self) -> Option<RistrettoPoint> {
        CompressedRistretto(self.token).decompress()
    }

    /// Serialize a voucher losslessly as 104 raw bytes.
    pub fn to_bytes(&self) -> [u8; 104] {
        let mut out = [0u8; 104];
        out[0..8].copy_from_slice(&self.amount.to_le_bytes());
        out[8..40].copy_from_slice(&self.token);
        out[40..72].copy_from_slice(&self.challenge);
        out[72..104].copy_from_slice(&self.response);
        out
    }

    /// Deserialize a voucher from its 104-byte encoding.
    pub fn from_bytes(bytes: &[u8; 104]) -> Self {
        let mut amount_bytes = [0u8; 8];
        let mut token = [0u8; 32];
        let mut challenge = [0u8; 32];
        let mut response = [0u8; 32];
        amount_bytes.copy_from_slice(&bytes[0..8]);
        token.copy_from_slice(&bytes[8..40]);
        challenge.copy_from_slice(&bytes[40..72]);
        response.copy_from_slice(&bytes[72..104]);
        Self {
            amount: u64::from_le_bytes(amount_bytes),
            token,
            challenge,
            response,
        }
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.to_bytes())
    }
}
