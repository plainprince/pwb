//! Sharded peer-attested credit ledger, $k$-of-$N$ neighborhoods (§4.2).
//!
//! Authoritative credit history does **not** live with the earning node. Each
//! node's balance is attested and hash-chained by $N$ deterministic peer
//! *attestors*, chosen every epoch by ranking candidate peers under the shard
//! key $\text{BLAKE3}(\text{NodeID} \parallel \text{Epoch})$ (XOR metric, so
//! membership rotates with the epoch and cannot be parked by a Sybil).
//! The earning node only holds a local hint (`identity.credits`); spendable
//! authority is what the neighborhood attests ([`QuorumCertificate`]).
//!
//! # Attestor view
//!
//! An [`AttestorLedger`] stores, per subject, a tamper-evident chain of
//! [`CreditEvent`]s (`Earned`/`Spent` with evidence digests). Recording an
//! event for your own identity is refused ([`LedgerError::SelfAttestation`])
//! — matching the spec that nodes never author their own balance. Index
//! earnings are re-verified end-to-end against the actual bundle bytes before
//! they extend the chain.
//!
//! # Claiming with $k$-of-$N$
//!
//! [`BalanceAttestation`] is one attestor's signed statement of a subject's
//! balance and chain head. A [`QuorumCertificate`] is $k$ of the neighborhood's
//! attestations over the *same* statement; verification enforces quorum size,
//! signer membership in the deterministic neighborhood, signer distinctness,
//! and signature validity. (Without new dependencies this is a verifiable
//! quorum of per-attestor Ed25519 signatures — any $k$ members must
//! independently vouch, which is what the $k$-of-$N$ property needs.)
//! `register.plain` claims go through [`crate::domain::DomainRegistry::register_attested`],
//! which also prevents replaying one certificate across claims via per-subject
//! claimed-spend accounting.

mod attest;
mod attestor;
mod chain;
mod error;
mod event;

#[cfg(test)]
mod tests;

/// Domain-separation prefix for every ledger-signed message and chain fold.
pub const LEDGER_SIGN_PREFIX: &[u8] = b"PWBLEDGER1";

/// Default attestor neighborhood size $N$.
pub const DEFAULT_ATTESTOR_N: usize = 8;

/// Default quorum threshold $k$ (must satisfy $1 \le k \le N$).
pub const DEFAULT_QUORUM_K: usize = 5;

pub use attest::{BalanceAttestation, QuorumCertificate};
pub use attestor::AttestorLedger;
pub use chain::{
    chain_head_after, epoch_at, epoch_key, genesis_head, select_attestors, verify_history,
};
pub use error::LedgerError;
pub use event::{ChainedEvent, CreditEvent, EventKind};
