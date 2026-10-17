//! Blind balance & privacy preservation (§4.3).
//!
//! Attestors must be able to log credit *volume* without linking a node owner
//! to specific browsing actions or domain claims. This module layers the
//! Chaum-style blind signature vouchers of [`pwb_core::crypto`] over the §4.2
//! attested ledger:
//!
//! - **Withdrawal** — a node converts attested credits into unlinkable blind
//!   vouchers. The attestor-issuer ([`BlindBalanceIssuer`]) checks the
//!   subject's attested balance, signs *blinded* requests with
//!   denomination-bound keys (so amounts cannot be forged), and records a
//!   `Spent` volume event on its [`AttestorLedger`]. The issuer never learns
//!   the voucher tokens: only fresh blinding factors cross the wire.
//! - **Redemption** — the holder spends a voucher (e.g. on `register.plain`)
//!   by presenting it with a zero-knowledge
//!   [`VoucherOwnershipProof`](pwb_core::crypto::VoucherOwnershipProof). The
//!   registry sees only the bearer token: a spend is unlinkable to the
//!   withdrawal, to the earning identity, and to any other spend.
//!
//! # Cross-attestor coordination
//!
//! A subject's balance is attested by $N$ peers, so each attestor's ledger
//! shows the same earnings. Withdrawing from one attestor must therefore
//! *gossip* the spend event (same [`withdrawal_evidence`] digest) to the rest
//! of the neighborhood — otherwise each attestor would independently back a
//! fresh withdrawal and the $N$ views would inflate the spendable supply.
//! [`withdraw_attested`] records the spend on the ledger it is given; the
//! caller repeats [`AttestorLedger::record_spent`] with the returned
//! [`WithdrawalReceipt::evidence`] on the sibling ledgers.

mod claim;
mod error;
mod issuer;
mod withdraw;

#[cfg(test)]
mod tests;

/// Domain-separation prefix for issuer-binding signatures and withdrawal
/// evidence digests.
const BLIND_PREFIX: &[u8] = b"PWBBLIND1";

/// Default denominations a [`BlindBalanceIssuer`] supports (credits).
pub const DEFAULT_DENOMINATIONS: &[u64] = &[1, 5, 10, 25, 50];

pub use claim::BlindClaim;
pub use error::{BlindClaimError, BlindError};
pub use issuer::{BlindBalanceIssuer, IssuerBinding};
pub use withdraw::{
    withdraw_attested, withdrawal_evidence, BlindWithdrawal, WithdrawalReceipt, WithdrawnVoucher,
};
