//! A complete private domain claim (voucher + ownership proof + binding).

use pwb_core::crypto::{BlindVoucher, VoucherOwnershipProof};

use super::issuer::IssuerBinding;
use super::withdraw::WithdrawnVoucher;

/// A complete private domain claim: the bearer voucher, its zero-knowledge
/// ownership proof, and the issuer binding tying the denomination key to a
/// neighborhood attestor.
#[derive(Debug, Clone)]
pub struct BlindClaim {
    /// The voucher being redeemed.
    pub voucher: BlindVoucher,
    /// Zero-knowledge proof that the redeemer holds the voucher's secret.
    pub proof: VoucherOwnershipProof,
    /// The attestor-signed denomination-key binding.
    pub binding: IssuerBinding,
}

impl BlindClaim {
    /// Build a claim from a withdrawn voucher and its binding. Returns `None`
    /// when the supplied secret does not open the voucher's token.
    pub fn build(withdrawn: &WithdrawnVoucher, binding: &IssuerBinding) -> Option<Self> {
        let proof = withdrawn.voucher.prove_ownership(&withdrawn.secret)?;
        Some(BlindClaim {
            voucher: withdrawn.voucher.clone(),
            proof,
            binding: binding.clone(),
        })
    }
}
