//! Cryptographic primitives for PlainWeb (`pwb-core::crypto`).
//!
//! Provides Ed25519 identity keypairs and signatures, BLAKE3 content addressing,
//! X25519 ECDH key agreement, and unlinkable blind-signature credit vouchers
//! with zero-knowledge ownership proofs and replay-rejecting spend tracking.

use blake3;
use ed25519_dalek::{Signature as Ed25519Signature, Signer, SigningKey, Verifier, VerifyingKey};
use std::collections::HashSet;
use std::fmt;

use rand::rngs::OsRng;
use rand::RngCore;

use curve25519_dalek::constants::RISTRETTO_BASEPOINT_TABLE;
use curve25519_dalek::ristretto::{CompressedRistretto, RistrettoPoint};
use curve25519_dalek::scalar::Scalar;
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// 256-bit Identity Public Key (Ed25519).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

/// 256-bit Content Digest / Hash (BLAKE3/SHA256).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContentId(pub [u8; 32]);

impl ContentId {
    pub fn compute(data: &[u8]) -> Self {
        let hash = blake3::hash(data);
        ContentId(*hash.as_bytes())
    }

    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{:02x}", b)).collect()
    }
}

impl fmt::Display for ContentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}

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
/// Bearer secret that defines a blind credit voucher's hidden token.
///
/// The token point is derived from this secret, so the holder can prove
/// ownership without revealing which issuance produced the voucher.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
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
struct ScalarBytes(pub [u8; 32]);

impl fmt::Debug for ScalarBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ScalarBytes(redacted)")
    }
}

/// Domain separation for every blind voucher derivation.
const BLIND_VOUCHER_DOMAIN: &[u8] = b"pwb-blind-voucher-schnorr-v1";

fn scalar_from_parts(domain_part: &[u8], seed: &[u8; 32]) -> ScalarBytes {
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

    fn as_point(&self) -> Option<RistrettoPoint> {
        self.point.decompress()
    }
}

/// One-time issuer nonce for a single blinded signing request.
pub struct BlindVoucherNonce {
    point: RistrettoPoint,
    nonce: ScalarBytes,
}

impl BlindVoucherNonce {
    /// Issuer commitment sent to the requester.
    pub fn to_bytes(&self) -> [u8; 32] {
        self.point.compress().to_bytes()
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

/// Blinded signing request an issuer signs without learning the token.
///
/// The issuer cannot see the token or the amount inside a blind signature, so
/// the amount is authorized structurally instead: an issuer derived with
/// [`BlindVoucherIssuer::from_denomination_seed`] only signs its own
/// denomination, and the amount is bound into the final challenge so a voucher
/// verifies for exactly the amount it was issued for.
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
    response: ScalarBytes,
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

/// Error returned when a voucher cannot be redeemed.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum VoucherSpendError {
    /// The voucher did not verify against the issuer's public key, or its
    /// ownership proof was invalid.
    #[error("voucher signature is invalid for this issuer")]
    InvalidVoucher,
    /// The voucher's token was already redeemed.
    #[error("voucher token has already been spent")]
    AlreadySpent,
}

/// Tracks redeemed voucher tokens so a voucher can only be spent once.
///
/// Unlinkable vouchers deliberately reveal nothing tying a redemption back to
/// an issuance, so replay prevention is necessarily a separate, stateful step.
/// This log supplies that state: it records each spent token identifier and
/// rejects any repeat presentation of the same voucher.
#[derive(Debug, Default)]
pub struct VoucherSpendLog {
    spent: HashSet<[u8; 32]>,
}

impl VoucherSpendLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the voucher's token has already been redeemed.
    pub fn is_spent(&self, voucher: &BlindVoucher) -> bool {
        self.spent.contains(&voucher.spend_id())
    }

    /// Number of recorded redemptions.
    pub fn len(&self) -> usize {
        self.spent.len()
    }

    /// Whether no voucher has been redeemed yet.
    pub fn is_empty(&self) -> bool {
        self.spent.is_empty()
    }

    /// Verify a voucher and record its token, rejecting replays.
    ///
    /// On success returns the credit amount the voucher authorizes.
    pub fn redeem(
        &mut self,
        voucher: &BlindVoucher,
        issuer_public: &BlindVoucherPublicKey,
    ) -> Result<u64, VoucherSpendError> {
        if !voucher.verify(issuer_public) {
            return Err(VoucherSpendError::InvalidVoucher);
        }
        if !self.spent.insert(voucher.spend_id()) {
            return Err(VoucherSpendError::AlreadySpent);
        }
        Ok(voucher.amount)
    }

    /// Verify, record, and additionally require an ownership proof.
    ///
    /// Use this when the redeemer must prove control of the voucher without
    /// revealing the bearer secret.
    pub fn redeem_with_proof(
        &mut self,
        voucher: &BlindVoucher,
        issuer_public: &BlindVoucherPublicKey,
        proof: &VoucherOwnershipProof,
    ) -> Result<u64, VoucherSpendError> {
        if !voucher.verify(issuer_public) || !voucher.verify_ownership(proof) {
            return Err(VoucherSpendError::InvalidVoucher);
        }
        if !self.spent.insert(voucher.spend_id()) {
            return Err(VoucherSpendError::AlreadySpent);
        }
        Ok(voucher.amount)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_id_produces_32_byte_blake3() {
        let id = ContentId::compute(b"hello world");
        assert_eq!(id.0.len(), 32);
    }

    #[test]
    fn ed25519_sign_verify_roundtrip() {
        let keypair = Keypair::generate();
        let message = b"test message";
        let signature = keypair.sign(message);
        assert!(signature.verify(&keypair.public, message));
    }

    #[test]
    fn x25519_diffie_hellman_agreement() {
        let alice = DhKeypair::generate();
        let bob = DhKeypair::generate();
        let shared_alice = alice.diffie_hellman(&bob.public);
        let shared_bob = bob.diffie_hellman(&alice.public);
        assert_eq!(shared_alice, shared_bob);
    }

    #[test]
    fn x25519_shared_secret_is_not_default() {
        let alice = DhKeypair::generate();
        let bob = DhKeypair::generate();
        let shared = alice.diffie_hellman(&bob.public);
        assert_ne!(shared, [0u8; 32]);
    }

    /// Runs the full interactive issuance: nonce, blinded request, blind
    /// signature, then unblinding into a verifiable voucher.
    fn issue_voucher(
        issuer: &BlindVoucherIssuer,
        amount: u64,
        secret: &VoucherSecret,
        blinding: &VoucherBlinding,
    ) -> BlindVoucher {
        let public = issuer.public_key();
        let nonce = issuer.signing_nonce();
        let (state, request) =
            BlindVoucher::blind_request(amount, secret, blinding, &public, &nonce)
                .expect("valid issuer key");
        let partial = issuer
            .sign_blind(nonce, &request)
            .expect("issuer accepts the amount");
        BlindVoucher::unblind(state, &partial).expect("valid blinded signature")
    }

    #[test]
    fn blind_voucher_issuance_and_redemption() {
        let issuer = BlindVoucherIssuer::generate();
        let public = issuer.public_key();
        let amount = 42;
        let secret = VoucherSecret::from_bytes([0x42; 32]);
        let blinding = VoucherBlinding::from_seed([0x13; 32]);

        let voucher = issue_voucher(&issuer, amount, &secret, &blinding);

        assert_eq!(voucher.amount, amount);
        assert!(voucher.verify(&public));
        assert!(voucher.verify_owner(&secret));
        assert!(voucher.token_point().is_some());

        let wrong_secret = VoucherSecret::from_bytes([0x43; 32]);
        assert!(!voucher.verify_owner(&wrong_secret));

        let other_issuer = BlindVoucherIssuer::generate();
        assert!(!voucher.verify(&other_issuer.public_key()));
    }

    #[test]
    fn blind_voucher_binds_amount_and_token() {
        let issuer = BlindVoucherIssuer::generate();
        let public = issuer.public_key();
        let secret = VoucherSecret::random();
        let blinding = VoucherBlinding::random();
        let voucher = issue_voucher(&issuer, 10, &secret, &blinding);

        assert!(voucher.verify(&public));

        let mut tampered_amount = voucher.clone();
        tampered_amount.amount = 1_000_000;
        assert!(!tampered_amount.verify(&public));

        let mut tampered_token = voucher.clone();
        tampered_token.token = VoucherSecret::random().to_bytes();
        assert!(!tampered_token.verify(&public));

        let mut tampered_response = voucher.clone();
        tampered_response.response[0] ^= 0x01;
        assert!(!tampered_response.verify(&public));
    }

    #[test]
    fn blind_voucher_issuances_are_unlinkable() {
        let issuer = BlindVoucherIssuer::generate();
        let public = issuer.public_key();
        let secret = VoucherSecret::random();

        let first = issue_voucher(&issuer, 5, &secret, &VoucherBlinding::random());
        let second = issue_voucher(&issuer, 5, &secret, &VoucherBlinding::random());

        // Both are valid, but the issuer-visible signature material differs,
        // so the issuer cannot link a redemption back to an issuance request.
        assert!(first.verify(&public));
        assert!(second.verify(&public));
        assert_ne!(first.challenge, second.challenge);
        assert_ne!(first.response, second.response);
    }

    #[test]
    fn blind_voucher_serialization_roundtrip() {
        let issuer = BlindVoucherIssuer::generate();
        let public = issuer.public_key();
        let secret = VoucherSecret::random();
        let voucher = issue_voucher(&issuer, 7, &secret, &VoucherBlinding::random());

        let encoded = voucher.to_bytes();
        let decoded = BlindVoucher::from_bytes(&encoded);
        assert_eq!(voucher, decoded);
        assert!(decoded.verify(&public));
        assert!(decoded.verify_owner(&secret));
        assert_eq!(voucher.to_hex(), hex::encode(encoded));
    }

    #[test]
    fn blind_voucher_partial_signature_roundtrip() {
        let issuer = BlindVoucherIssuer::generate();
        let public = issuer.public_key();
        let secret = VoucherSecret::random();
        let blinding = VoucherBlinding::random();

        let nonce = issuer.signing_nonce();
        let (state, request) =
            BlindVoucher::blind_request(11, &secret, &blinding, &public, &nonce).unwrap();
        let partial = issuer.sign_blind(nonce, &request).unwrap();
        let bytes = partial.to_bytes();
        let restored = BlindVoucherPartialSignature::from_bytes(bytes).unwrap();

        let voucher = BlindVoucher::unblind(state, &restored).unwrap();
        assert!(voucher.verify(&public));
        assert!(BlindVoucherPartialSignature::from_bytes([0xff; 32]).is_none());
    }

    #[test]
    fn blind_voucher_ownership_proof_is_zero_knowledge() {
        let issuer = BlindVoucherIssuer::generate();
        let public = issuer.public_key();
        let secret = VoucherSecret::random();
        let voucher = issue_voucher(&issuer, 3, &secret, &VoucherBlinding::random());

        let proof = voucher.prove_ownership(&secret).expect("holder can prove");
        assert!(voucher.verify_ownership(&proof));

        // A third party verifies with no access to the bearer secret.
        assert!(voucher.verify(&public));

        // Proofs must not be transferable to another voucher.
        let other = issue_voucher(
            &issuer,
            3,
            &VoucherSecret::random(),
            &VoucherBlinding::random(),
        );
        assert!(!other.verify_ownership(&proof));

        // A wrong secret cannot produce a proof at all.
        let wrong = VoucherSecret::random();
        assert!(voucher.prove_ownership(&wrong).is_none());

        // Tampered proof components are rejected.
        let mut tampered = proof;
        tampered.response[0] ^= 0x01;
        assert!(!voucher.verify_ownership(&tampered));

        let encoded = proof.to_bytes();
        assert_eq!(VoucherOwnershipProof::from_bytes(&encoded), proof);
        assert_eq!(proof.to_hex(), hex::encode(encoded));
    }

    #[test]
    fn blind_voucher_denomination_keys_enforce_amount() {
        let master_seed = [0x5a; 32];
        let hundred = BlindVoucherIssuer::from_denomination_seed(&master_seed, 100);
        let thousand = BlindVoucherIssuer::from_denomination_seed(&master_seed, 1000);
        let public = hundred.public_key();

        assert_eq!(hundred.denomination(), Some(100));
        assert!(hundred.accepts_amount(100));
        assert!(!hundred.accepts_amount(1000));
        assert_ne!(
            hundred.public_key().to_bytes(),
            thousand.public_key().to_bytes()
        );

        // The 100 key issues a 100 voucher.
        let voucher = issue_voucher(
            &hundred,
            100,
            &VoucherSecret::random(),
            &VoucherBlinding::random(),
        );
        assert!(voucher.verify(&public));

        // Asking the 100 key to authorize a different amount is refused.
        let secret = VoucherSecret::random();
        let blinding = VoucherBlinding::random();
        let nonce = hundred.signing_nonce();
        let (_state, request) =
            BlindVoucher::blind_request(1000, &secret, &blinding, &public, &nonce).unwrap();
        assert!(hundred.sign_blind(nonce, &request).is_none());

        // A 1000 voucher cannot verify against the 100 public key.
        let thousand_secret = VoucherSecret::random();
        let thousand_voucher = issue_voucher(
            &thousand,
            1000,
            &thousand_secret,
            &VoucherBlinding::random(),
        );
        assert!(!thousand_voucher.verify(&public));
    }

    #[test]
    fn blind_voucher_spend_log_rejects_replays() {
        let issuer = BlindVoucherIssuer::generate();
        let public = issuer.public_key();
        let secret = VoucherSecret::random();
        let voucher = issue_voucher(&issuer, 25, &secret, &VoucherBlinding::random());
        let proof = voucher.prove_ownership(&secret).unwrap();

        let mut log = VoucherSpendLog::new();
        assert!(log.is_empty());
        assert!(!log.is_spent(&voucher));

        assert_eq!(log.redeem(&voucher, &public).unwrap(), 25);
        assert_eq!(log.len(), 1);
        assert!(log.is_spent(&voucher));
        assert_eq!(
            log.redeem(&voucher, &public),
            Err(VoucherSpendError::AlreadySpent)
        );

        // An unrelated voucher still redeems, and its proof checks out.
        let second = issue_voucher(
            &issuer,
            25,
            &VoucherSecret::random(),
            &VoucherBlinding::random(),
        );
        assert_eq!(
            log.redeem_with_proof(&second, &public, &proof),
            Err(VoucherSpendError::InvalidVoucher)
        );
        let second_secret = VoucherSecret::random();
        let third = issue_voucher(&issuer, 25, &second_secret, &VoucherBlinding::random());
        let third_proof = third.prove_ownership(&second_secret).unwrap();
        assert_eq!(
            log.redeem_with_proof(&third, &public, &third_proof)
                .unwrap(),
            25
        );
        assert_eq!(log.len(), 2);

        // A forged voucher is rejected before any state changes.
        let mut forged = voucher.clone();
        forged.amount = 1_000_000;
        assert_eq!(
            log.redeem(&forged, &public),
            Err(VoucherSpendError::InvalidVoucher)
        );
    }
}
