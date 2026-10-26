//! Ledger error type.

use std::fmt;

/// Why a ledger operation was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerError {
    /// A node may not author credit history or attestations for itself.
    SelfAttestation,
    /// An event's epoch went backwards for this subject.
    EpochRegression { previous: u64, incoming: u64 },
    /// The same evidence was already recorded for this subject.
    DuplicateEvent,
    /// A spend exceeds the subject's attested balance.
    InsufficientBalance { balance: u64, needed: u64 },
    /// The index receipt failed signature/content/term verification.
    ReceiptInvalid,
    /// The index receipt's bundle bytes do not deserialize.
    UnreadableBundle,
    /// Fewer than `need` attestations were supplied.
    QuorumTooSmall { have: usize, need: usize },
    /// An attestor is not a member of the subject's neighborhood.
    SignerNotInNeighborhood,
    /// The same attestor signed twice.
    DuplicateSigner,
    /// Attestations do not all cover the same statement.
    AttestationMismatch,
    /// An attestation's signature does not verify.
    BadAttestationSignature,
    /// Attested balance minus already-claimed credits cannot cover the cost.
    InsufficientAttestedBalance {
        attested: u64,
        already_claimed: u64,
        needed: u64,
    },
    /// The `.plain` TLD is reserved for built-in system protocols.
    ReservedSystemDomain,
    /// The name carries no TLD from the unofficial namespace.
    UnknownTld,
    /// The domain is already registered.
    DomainAlreadyRegistered,
}

impl fmt::Display for LedgerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LedgerError::SelfAttestation => {
                write!(f, "nodes cannot attest or record their own balance")
            }
            LedgerError::EpochRegression {
                previous,
                incoming,
            } => write!(
                f,
                "event epoch {incoming} regresses behind last recorded epoch {previous}"
            ),
            LedgerError::DuplicateEvent => {
                write!(f, "evidence was already recorded for this subject")
            }
            LedgerError::InsufficientBalance { balance, needed } => {
                write!(f, "attested balance {balance} cannot cover spend of {needed}")
            }
            LedgerError::ReceiptInvalid => write!(f, "index receipt failed verification"),
            LedgerError::UnreadableBundle => write!(f, "bundle bytes do not deserialize"),
            LedgerError::QuorumTooSmall { have, need } => {
                write!(f, "quorum has {have} attestations, needs {need}")
            }
            LedgerError::SignerNotInNeighborhood => {
                write!(f, "attestor is not in the subject's neighborhood")
            }
            LedgerError::DuplicateSigner => write!(f, "the same attestor signed twice"),
            LedgerError::AttestationMismatch => {
                write!(f, "attestations do not cover the same statement")
            }
            LedgerError::BadAttestationSignature => {
                write!(f, "attestation signature is invalid")
            }
            LedgerError::InsufficientAttestedBalance {
                attested,
                already_claimed,
                needed,
            } => write!(
                f,
                "attested balance {attested} minus {already_claimed} already claimed cannot cover {needed}"
            ),
            LedgerError::ReservedSystemDomain => {
                write!(f, "reserved system domain: .plain TLD is restricted")
            }
            LedgerError::UnknownTld => write!(f, "{}", pwb_core::system::unknown_tld_message()),
            LedgerError::DomainAlreadyRegistered => write!(f, "domain name already registered"),
        }
    }
}

impl std::error::Error for LedgerError {}
