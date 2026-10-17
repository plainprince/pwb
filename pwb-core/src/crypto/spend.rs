//! Voucher redemption tracking and spend errors.

use std::collections::HashSet;

use super::issuer::BlindVoucherPublicKey;
use super::voucher::{BlindVoucher, VoucherOwnershipProof};

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
#[derive(Debug, Default, Clone, PartialEq, Eq)]
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

    /// Union another spend log into this one (gossip partition merge).
    ///
    /// A token spent on either side of an isolated partition is spent
    /// everywhere after the merge, so a bearer voucher cannot be redeemed
    /// twice across the re-joined network. Commutative, associative,
    /// idempotent.
    pub fn merge(&mut self, other: &VoucherSpendLog) {
        for token in &other.spent {
            self.spent.insert(*token);
        }
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
