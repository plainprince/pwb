//! Wire conversions for blind vouchers, ownership proofs, and issuer bindings.
//!
//! Both sides of these conversions are foreign types (`pwb_core::crypto` and
//! `pwb_core::net`), so they are explicit functions rather than `From` impls
//! (which the orphan rules forbid in `pwb-node`).

use pwb_core::crypto::{
    BlindVoucher, BlindVoucherPublicKey, VoucherBlindRequest, VoucherOwnershipProof,
};
use pwb_core::net::{
    BlindClaimWire, IssuerBindingWire, OwnershipProofWire, VoucherBlindRequestWire, VoucherWire,
};

use super::issuer::IssuerBinding;
use super::BlindClaim;

pub(crate) fn voucher_to_wire(voucher: &BlindVoucher) -> VoucherWire {
    VoucherWire {
        amount: voucher.amount,
        token: voucher.token,
        challenge: voucher.challenge,
        response: voucher.response,
    }
}

pub(crate) fn voucher_from_wire(wire: &VoucherWire) -> BlindVoucher {
    BlindVoucher {
        amount: wire.amount,
        token: wire.token,
        challenge: wire.challenge,
        response: wire.response,
    }
}

pub(crate) fn proof_to_wire(proof: &VoucherOwnershipProof) -> OwnershipProofWire {
    OwnershipProofWire {
        commitment: proof.commitment,
        response: proof.response,
    }
}

pub(crate) fn proof_from_wire(wire: &OwnershipProofWire) -> VoucherOwnershipProof {
    VoucherOwnershipProof {
        commitment: wire.commitment,
        response: wire.response,
    }
}

pub(crate) fn binding_to_wire(binding: &IssuerBinding) -> IssuerBindingWire {
    IssuerBindingWire {
        attestor: binding.attestor,
        epoch: binding.epoch,
        denomination: binding.denomination,
        voucher_key: binding.voucher_key.to_bytes(),
        signature: binding.signature,
    }
}

pub(crate) fn binding_from_wire(wire: &IssuerBindingWire) -> Option<IssuerBinding> {
    let voucher_key = BlindVoucherPublicKey::from_bytes(wire.voucher_key)?;
    Some(IssuerBinding {
        attestor: wire.attestor,
        epoch: wire.epoch,
        denomination: wire.denomination,
        voucher_key,
        signature: wire.signature,
    })
}

pub(crate) fn claim_to_wire(claim: &BlindClaim) -> BlindClaimWire {
    BlindClaimWire {
        voucher: voucher_to_wire(&claim.voucher),
        proof: proof_to_wire(&claim.proof),
        binding: binding_to_wire(&claim.binding),
    }
}

pub(crate) fn claim_from_wire(wire: &BlindClaimWire) -> Option<BlindClaim> {
    Some(BlindClaim {
        voucher: voucher_from_wire(&wire.voucher),
        proof: proof_from_wire(&wire.proof),
        binding: binding_from_wire(&wire.binding)?,
    })
}

pub(crate) fn request_to_wire(request: &VoucherBlindRequest) -> VoucherBlindRequestWire {
    VoucherBlindRequestWire {
        amount: request.amount,
        blinded_challenge: request.blinded_challenge,
    }
}

pub(crate) fn request_from_wire(wire: &VoucherBlindRequestWire) -> VoucherBlindRequest {
    VoucherBlindRequest {
        amount: wire.amount,
        blinded_challenge: wire.blinded_challenge,
    }
}

/// Rebuild and verify an [`IssuerBinding`] from its wire form, returning `None`
/// when the key encoding or attestor signature is invalid.
pub(crate) fn verified_binding(wire: &IssuerBindingWire) -> Option<IssuerBinding> {
    let binding = binding_from_wire(wire)?;
    binding.verify().then_some(binding)
}

/// Sign a new issuer binding for the attestation service.
pub(crate) fn sign_binding(
    attestor: &pwb_core::crypto::Keypair,
    epoch: u64,
    denomination: u64,
    voucher_key: BlindVoucherPublicKey,
) -> IssuerBinding {
    IssuerBinding::sign(attestor, epoch, denomination, voucher_key)
}
