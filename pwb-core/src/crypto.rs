//! Cryptographic primitives for PlainWeb (`pwb-core::crypto`).
//!
//! Provides Ed25519 identity keypairs and signatures, BLAKE3 content addressing,
//! X25519 ECDH key agreement, and unlinkable blind-signature credit vouchers
//! with zero-knowledge ownership proofs and replay-rejecting spend tracking.

mod content;
mod dh;
mod identity;
mod issuer;
mod spend;
mod voucher;

#[cfg(test)]
mod tests;

pub use content::ContentId;
pub use dh::{DhKeypair, DhPublicKey, DhSecretKey};
pub use identity::{Keypair, PublicKey, SecretKey, Signature};
pub use issuer::{BlindVoucherIssuer, BlindVoucherNonce, BlindVoucherPublicKey};
pub use spend::{VoucherSpendError, VoucherSpendLog};
pub use voucher::{
    BlindVoucher, BlindVoucherPartialSignature, VoucherBlindRequest, VoucherBlindState,
    VoucherBlinding, VoucherOwnershipProof, VoucherSecret,
};
