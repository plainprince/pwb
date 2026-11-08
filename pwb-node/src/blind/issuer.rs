//! Blind-signing issuer keys and attestor-issued denomination bindings.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::time::{Duration, Instant};

use pwb_core::crypto::{
    BlindVoucherIssuer, BlindVoucherNonce, BlindVoucherPartialSignature, BlindVoucherPublicKey,
    Keypair, PublicKey, Signature, VoucherBlindRequest,
};

use super::error::BlindError;
use crate::blind::{BLIND_PREFIX, DEFAULT_DENOMINATIONS};

/// How long an unclaimed nonce batch stays usable. A withdrawal that is not
/// completed inside this window is abandoned; keeping its nonces around would
/// let one slow or hostile requester wedge every later batch.
const PENDING_TTL: Duration = Duration::from_secs(120);

/// Upper bound on concurrent nonce batches, so a peer cycling request ids
/// cannot grow the pending map without bound.
const MAX_PENDING_BATCHES: usize = 256;

/// One committed, not-yet-signed nonce batch, keyed by request id.
struct PendingBatch {
    nonces: VecDeque<(u64, BlindVoucherNonce)>,
    created: Instant,
}

/// One attestor's blind-signing capability: denomination-bound issuer keys
/// derived from a master seed, plus the single-use nonce queues pairing
/// committed nonces with incoming blinded requests.
///
/// Batches are keyed by the caller's request id (`query_id` on the wire) so
/// one requester's abandoned withdrawal cannot poison another's. A batch's
/// nonces are consumed only by a successful [`Self::sign_batch`]; a failed
/// batch is cleared by the caller or expires after [`PENDING_TTL`].
///
/// The master seed is the issuer's *blind-signing* secret — distinct from the
/// attestor's Ed25519 identity key, which only signs [`IssuerBinding`]s.
pub struct BlindBalanceIssuer {
    issuers: HashMap<u64, BlindVoucherIssuer>,
    pending: HashMap<u64, PendingBatch>,
}

impl BlindBalanceIssuer {
    /// Issuer supporting `denominations`, each key derived from `master_seed`
    /// via the domain-separated denomination derivation in `pwb-core`
    /// (distinct denominations yield independent, unlinkable keys).
    pub fn new(master_seed: &[u8; 32], denominations: &[u64]) -> Self {
        let issuers = denominations
            .iter()
            .map(|denomination| {
                (
                    *denomination,
                    BlindVoucherIssuer::from_denomination_seed(master_seed, *denomination),
                )
            })
            .collect();
        BlindBalanceIssuer {
            issuers,
            pending: HashMap::new(),
        }
    }

    /// Issuer with [`DEFAULT_DENOMINATIONS`].
    pub fn with_defaults(master_seed: &[u8; 32]) -> Self {
        Self::new(master_seed, DEFAULT_DENOMINATIONS)
    }

    /// Verification key for a denomination, if supported.
    pub fn public_key(&self, denomination: u64) -> Option<BlindVoucherPublicKey> {
        self.issuers
            .get(&denomination)
            .map(|issuer| issuer.public_key())
    }

    /// Supported denominations, ascending.
    pub fn denominations(&self) -> Vec<u64> {
        let mut denoms: Vec<u64> = self.issuers.keys().copied().collect();
        denoms.sort_unstable();
        denoms
    }

    /// Drop batches older than [`PENDING_TTL`]. Called before every commit and
    /// signing attempt, so an abandoned withdrawal frees its nonces.
    fn expire_pending(&mut self, now: Instant) {
        self.pending
            .retain(|_, batch| now.saturating_duration_since(batch.created) < PENDING_TTL);
    }

    /// Commit to `count` fresh single-use nonces for `denomination` and
    /// return their commitment points for the requester. Requests must later
    /// be submitted in the same order via [`sign_batch`] with the same
    /// `request_id`.
    ///
    /// A reused request id returns no points (a live batch is never extended),
    /// so a replay cannot obtain a second signature over the same nonce.
    pub fn commit_nonces(
        &mut self,
        request_id: u64,
        denomination: u64,
        count: usize,
    ) -> Vec<[u8; 32]> {
        let now = Instant::now();
        self.expire_pending(now);
        if count == 0 || self.pending.contains_key(&request_id) {
            return Vec::new();
        }
        if self.pending.len() >= MAX_PENDING_BATCHES {
            // Evict the batch closest to expiry rather than refusing all new
            // requesters once an attacker has filled the table.
            let oldest = self
                .pending
                .iter()
                .min_by_key(|(_, batch)| batch.created)
                .map(|(id, _)| *id);
            if let Some(oldest) = oldest {
                self.pending.remove(&oldest);
            }
        }
        let Some(issuer) = self.issuers.get(&denomination) else {
            return Vec::new();
        };
        let mut nonces = VecDeque::with_capacity(count);
        let mut points = Vec::with_capacity(count);
        for _ in 0..count {
            let nonce = issuer.signing_nonce();
            points.push(nonce.to_bytes());
            nonces.push_back((denomination, nonce));
        }
        self.pending.insert(
            request_id,
            PendingBatch {
                nonces,
                created: now,
            },
        );
        points
    }

    /// Sign blinded withdrawal requests, paired positionally with the
    /// previously committed nonces for `request_id`. Refuses any batch whose
    /// total exceeds `max_spendable` (the subject's attested balance) or whose
    /// amounts fall outside their paired denomination key. Validation failures
    /// do not consume the pending nonces; a caller that will not retry must
    /// call [`Self::clear_pending`].
    pub fn sign_batch(
        &mut self,
        request_id: u64,
        requests: &[VoucherBlindRequest],
        max_spendable: u64,
    ) -> Result<Vec<BlindVoucherPartialSignature>, BlindError> {
        self.expire_pending(Instant::now());
        let Some(batch) = self.pending.get(&request_id) else {
            return Err(BlindError::NonceCountMismatch);
        };
        if requests.len() != batch.nonces.len() {
            return Err(BlindError::NonceCountMismatch);
        }
        let total = requests
            .iter()
            .try_fold(0u64, |total, request| total.checked_add(request.amount));
        let Some(total) = total else {
            return Err(BlindError::InvalidAmount);
        };
        if total > max_spendable {
            return Err(BlindError::InsufficientBalance {
                balance: max_spendable,
                needed: total,
            });
        }
        for (request, (denomination, _)) in requests.iter().zip(&batch.nonces) {
            if request.amount != *denomination {
                return Err(BlindError::AmountMismatch);
            }
        }
        let batch = self
            .pending
            .remove(&request_id)
            .expect("batch was checked above");
        let mut partials = Vec::with_capacity(requests.len());
        for (request, (denomination, nonce)) in requests.iter().zip(batch.nonces) {
            let issuer = self
                .issuers
                .get(&denomination)
                .expect("pending nonces only come from supported denominations");
            let partial = issuer
                .sign_blind(nonce, request)
                .expect("amount already checked against the denomination key");
            partials.push(partial);
        }
        Ok(partials)
    }

    /// Discard a request's pending nonces. Used when the caller abandons the
    /// withdrawal (a failed or malformed second round).
    pub fn clear_pending(&mut self, request_id: u64) {
        self.pending.remove(&request_id);
    }

    /// Number of batches currently holding nonces (diagnostics/tests).
    pub fn pending_batches(&self) -> usize {
        self.pending.len()
    }
}

impl fmt::Debug for BlindBalanceIssuer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BlindBalanceIssuer")
            .field("denominations", &self.denominations())
            .field("pending_batches", &self.pending.len())
            .field(
                "pending_nonces",
                &self
                    .pending
                    .values()
                    .map(|batch| batch.nonces.len())
                    .sum::<usize>(),
            )
            .finish()
    }
}

/// An attestor's Ed25519-signed statement binding one of its blind-signing
/// denomination keys to its identity for an epoch. The registry trusts a
/// voucher only through a binding whose attestor is a neighborhood member —
/// so only *attested* credits are privately spendable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuerBinding {
    /// Attestor identity vouching for the key.
    pub attestor: PublicKey,
    /// Epoch the binding is valid for (keys rotate per epoch).
    pub epoch: u64,
    /// Denomination the key issues.
    pub denomination: u64,
    /// The blind-signing public key.
    pub voucher_key: BlindVoucherPublicKey,
    /// Attestor signature over the canonical binding encoding.
    pub signature: Signature,
}

impl IssuerBinding {
    /// Sign a binding with the attestor's identity keypair.
    pub fn sign(
        attestor: &Keypair,
        epoch: u64,
        denomination: u64,
        voucher_key: BlindVoucherPublicKey,
    ) -> Self {
        let message = binding_sign_message(attestor.public, epoch, denomination, voucher_key);
        IssuerBinding {
            attestor: attestor.public,
            epoch,
            denomination,
            voucher_key,
            signature: attestor.sign(&message),
        }
    }

    /// Verify the attestor signature over the canonical binding encoding.
    pub fn verify(&self) -> bool {
        let message = binding_sign_message(
            self.attestor,
            self.epoch,
            self.denomination,
            self.voucher_key,
        );
        self.signature.verify(&self.attestor, &message)
    }
}

fn binding_sign_message(
    attestor: PublicKey,
    epoch: u64,
    denomination: u64,
    voucher_key: BlindVoucherPublicKey,
) -> Vec<u8> {
    let mut msg = Vec::with_capacity(BLIND_PREFIX.len() + 48);
    msg.extend_from_slice(BLIND_PREFIX);
    msg.extend_from_slice(b"binding");
    msg.extend_from_slice(&attestor.0);
    msg.extend_from_slice(&epoch.to_le_bytes());
    msg.extend_from_slice(&denomination.to_le_bytes());
    msg.extend_from_slice(&voucher_key.to_bytes());
    msg
}
