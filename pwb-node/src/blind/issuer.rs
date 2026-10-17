//! Blind-signing issuer keys and attestor-issued denomination bindings.

use std::collections::{HashMap, VecDeque};
use std::fmt;

use pwb_core::crypto::{
    BlindVoucherIssuer, BlindVoucherNonce, BlindVoucherPartialSignature, BlindVoucherPublicKey,
    Keypair, PublicKey, Signature, VoucherBlindRequest,
};

use super::error::BlindError;
use crate::blind::{BLIND_PREFIX, DEFAULT_DENOMINATIONS};

/// One attestor's blind-signing capability: denomination-bound issuer keys
/// derived from a master seed, plus the single-use nonce queue pairing
/// committed nonces with incoming blinded requests.
///
/// The master seed is the issuer's *blind-signing* secret — distinct from the
/// attestor's Ed25519 identity key, which only signs [`IssuerBinding`]s.
pub struct BlindBalanceIssuer {
    issuers: HashMap<u64, BlindVoucherIssuer>,
    /// `(denomination, nonce)` in commit order; consumed by `sign_batch`.
    pending: VecDeque<(u64, BlindVoucherNonce)>,
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
            pending: VecDeque::new(),
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

    /// Commit to `count` fresh single-use nonces for `denomination` and
    /// return their commitment points for the requester. Requests must later
    /// be submitted in the same order via [`sign_batch`].
    pub fn commit_nonces(&mut self, denomination: u64, count: usize) -> Vec<[u8; 32]> {
        let Some(issuer) = self.issuers.get(&denomination) else {
            return Vec::new();
        };
        let mut points = Vec::with_capacity(count);
        for _ in 0..count {
            let nonce = issuer.signing_nonce();
            points.push(nonce.to_bytes());
            self.pending.push_back((denomination, nonce));
        }
        points
    }

    /// Sign blinded withdrawal requests, paired positionally with the
    /// previously committed nonces. Refuses any batch whose total exceeds
    /// `max_spendable` (the subject's attested balance) or whose amounts
    /// fall outside their paired denomination key. Validation failures do
    /// not consume the pending nonces.
    pub fn sign_batch(
        &mut self,
        requests: &[VoucherBlindRequest],
        max_spendable: u64,
    ) -> Result<Vec<BlindVoucherPartialSignature>, BlindError> {
        if requests.len() != self.pending.len() {
            return Err(BlindError::NonceCountMismatch);
        }
        let total: u64 = requests.iter().map(|r| r.amount).sum();
        if total > max_spendable {
            return Err(BlindError::InsufficientBalance {
                balance: max_spendable,
                needed: total,
            });
        }
        for (request, (denomination, _)) in requests.iter().zip(&self.pending) {
            if request.amount != *denomination {
                return Err(BlindError::AmountMismatch);
            }
        }
        let mut partials = Vec::with_capacity(requests.len());
        for (request, (denomination, nonce)) in requests.iter().zip(self.pending.drain(..)) {
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
}

impl fmt::Debug for BlindBalanceIssuer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BlindBalanceIssuer")
            .field("denominations", &self.denominations())
            .field("pending_nonces", &self.pending.len())
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
