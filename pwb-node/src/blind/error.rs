//! Blind-balance error types.

use std::fmt;

/// Why a blind-balance operation was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlindError {
    /// The requested amount is not a whole multiple of the denomination.
    InvalidAmount,
    /// The issuer does not support the requested denomination.
    UnknownDenomination(u64),
    /// The subject's attested balance cannot cover the withdrawal.
    InsufficientBalance { balance: u64, needed: u64 },
    /// The prepared requests do not match the committed nonces.
    NonceCountMismatch,
    /// A request's amount falls outside its paired denomination key.
    AmountMismatch,
    /// The unblinded voucher does not verify against the issuer key.
    BadVoucher,
    /// The holder cannot prove ownership (wrong secret).
    NoOwnershipProof,
    /// The attestor's ledger refused the volume event.
    LedgerRejected(String),
}

impl fmt::Display for BlindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BlindError::InvalidAmount => {
                write!(f, "amount is not a whole multiple of the denomination")
            }
            BlindError::UnknownDenomination(d) => write!(f, "unknown denomination {d}"),
            BlindError::InsufficientBalance { balance, needed } => {
                write!(
                    f,
                    "attested balance {balance} cannot cover withdrawal of {needed}"
                )
            }
            BlindError::NonceCountMismatch => {
                write!(f, "prepared requests do not match committed nonces")
            }
            BlindError::AmountMismatch => {
                write!(f, "request amount falls outside its denomination key")
            }
            BlindError::BadVoucher => write!(f, "unblinded voucher does not verify"),
            BlindError::NoOwnershipProof => write!(f, "cannot prove voucher ownership"),
            BlindError::LedgerRejected(reason) => {
                write!(f, "attestor ledger rejected withdrawal: {reason}")
            }
        }
    }
}

impl std::error::Error for BlindError {}

/// Why a blind domain claim was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlindClaimError {
    /// The `.plain` TLD is reserved for built-in system protocols.
    ReservedSystemDomain,
    /// The domain is already registered.
    DomainAlreadyRegistered,
    /// The issuer binding's signature does not verify.
    BadBinding,
    /// The binding's attestor is not a member of the claiming neighborhood.
    IssuerNotAttestor,
    /// The voucher does not verify against the bound denomination key.
    BadVoucher,
    /// The ownership proof is missing or invalid.
    NoOwnershipProof,
    /// The voucher's denomination is below the claim cost.
    Underfunded { voucher: u64, cost: u64 },
    /// The voucher's token was already redeemed.
    AlreadySpent,
}

impl fmt::Display for BlindClaimError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BlindClaimError::ReservedSystemDomain => {
                write!(f, "reserved system domain: .plain TLD is restricted")
            }
            BlindClaimError::DomainAlreadyRegistered => write!(f, "domain name already registered"),
            BlindClaimError::BadBinding => write!(f, "issuer binding signature is invalid"),
            BlindClaimError::IssuerNotAttestor => {
                write!(f, "binding attestor is not in the claiming neighborhood")
            }
            BlindClaimError::BadVoucher => {
                write!(f, "voucher does not verify against the bound key")
            }
            BlindClaimError::NoOwnershipProof => write!(f, "voucher ownership proof is invalid"),
            BlindClaimError::Underfunded { voucher, cost } => {
                write!(
                    f,
                    "voucher of {voucher} credits cannot cover claim cost {cost}"
                )
            }
            BlindClaimError::AlreadySpent => write!(f, "voucher token has already been spent"),
        }
    }
}

impl std::error::Error for BlindClaimError {}
