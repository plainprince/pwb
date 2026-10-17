//! One attestor's view of the subjects it vouches for.

use std::collections::HashMap;

use pwb_core::crypto::{ContentId, Keypair, PublicKey};
use pwb_core::wire::Bundle;

use super::attest::BalanceAttestation;
use super::chain::{chain_head_after, genesis_head, verify_history};
use super::error::LedgerError;
use super::event::{ChainedEvent, CreditEvent, EventKind};
use crate::credit::{canonical_term_hash, index_sign_message, IndexReceipt, MintPolicy};

/// One attestor node's view of the subjects it vouches for: a hash-chained
/// credit history per subject, never including itself.
#[derive(Debug, Clone)]
pub struct AttestorLedger {
    attestor: PublicKey,
    histories: HashMap<PublicKey, Vec<ChainedEvent>>,
}

impl AttestorLedger {
    /// Ledger for one attestor identity.
    pub fn new(attestor: PublicKey) -> Self {
        AttestorLedger {
            attestor,
            histories: HashMap::new(),
        }
    }

    /// The attestor identity whose statements this ledger produces.
    pub fn attestor(&self) -> PublicKey {
        self.attestor
    }

    /// Number of subjects with recorded history.
    pub fn subject_count(&self) -> usize {
        self.histories.len()
    }

    /// The subject's chained history, if anything was recorded.
    pub fn history(&self, subject: &PublicKey) -> &[ChainedEvent] {
        self.histories
            .get(subject)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Subject's current attested balance (0 when unknown).
    pub fn balance(&self, subject: &PublicKey) -> u64 {
        let history = self.history(subject);
        verify_history(subject, history).unwrap_or(0)
    }

    /// Subject's current chain head (genesis when unknown).
    pub fn head(&self, subject: &PublicKey) -> [u8; 32] {
        match self.histories.get(subject) {
            Some(history) => history
                .last()
                .map(|c| c.head)
                .unwrap_or_else(|| genesis_head(subject)),
            None => genesis_head(subject),
        }
    }

    /// Record verified earnings and return the new balance. The evidence
    /// digest must be unique per subject — a receipt can credit only once.
    pub fn record_earned(
        &mut self,
        subject: PublicKey,
        epoch: u64,
        amount: u64,
        evidence: [u8; 32],
    ) -> Result<u64, LedgerError> {
        self.record(subject, epoch, EventKind::Earned, amount, evidence)
    }

    /// Record a spend against the subject's attested balance.
    pub fn record_spent(
        &mut self,
        subject: PublicKey,
        epoch: u64,
        amount: u64,
        evidence: [u8; 32],
    ) -> Result<u64, LedgerError> {
        let balance = self.balance(&subject);
        if balance < amount {
            return Err(LedgerError::InsufficientBalance {
                balance,
                needed: amount,
            });
        }
        self.record(subject, epoch, EventKind::Spent, amount, evidence)
    }

    /// Record earnings from an index receipt after re-verifying its
    /// signature, the bundle's content id, and its canonical term hash
    /// against the actual bytes. The credited amount follows `policy`.
    pub fn record_index_earned(
        &mut self,
        receipt: &IndexReceipt,
        bundle_bytes: &[u8],
        epoch: u64,
        policy: &MintPolicy,
    ) -> Result<u64, LedgerError> {
        let message = index_sign_message(receipt.indexer, receipt.content_id, receipt.term_hash);
        if !receipt.signature.verify(&receipt.indexer, &message) {
            return Err(LedgerError::ReceiptInvalid);
        }
        if ContentId::compute(bundle_bytes) != receipt.content_id {
            return Err(LedgerError::ReceiptInvalid);
        }
        let bundle =
            Bundle::deserialize(bundle_bytes).map_err(|_| LedgerError::UnreadableBundle)?;
        if canonical_term_hash(&bundle) != receipt.term_hash {
            return Err(LedgerError::ReceiptInvalid);
        }
        self.record_earned(
            receipt.indexer,
            epoch,
            policy.index_credits,
            receipt.content_id.0,
        )
    }

    /// Sign a balance statement for `subject` at `epoch` with `keypair`,
    /// which must be this attestor's own key. Statements are allowed for
    /// unknown subjects (balance 0, genesis head).
    pub fn attest(
        &self,
        subject: PublicKey,
        epoch: u64,
        keypair: &Keypair,
    ) -> Result<BalanceAttestation, LedgerError> {
        if keypair.public != self.attestor {
            return Err(LedgerError::SelfAttestation);
        }
        if subject == self.attestor {
            return Err(LedgerError::SelfAttestation);
        }
        let balance = self.balance(&subject);
        let head = self.head(&subject);
        Ok(BalanceAttestation::sign(
            keypair, subject, epoch, balance, head,
        ))
    }

    fn record(
        &mut self,
        subject: PublicKey,
        epoch: u64,
        kind: EventKind,
        amount: u64,
        evidence: [u8; 32],
    ) -> Result<u64, LedgerError> {
        if subject == self.attestor {
            return Err(LedgerError::SelfAttestation);
        }
        let history = self.histories.entry(subject).or_default();
        if let Some(last) = history.last() {
            if epoch < last.event.epoch {
                return Err(LedgerError::EpochRegression {
                    previous: last.event.epoch,
                    incoming: epoch,
                });
            }
            if history
                .iter()
                .any(|chained| chained.event.evidence == evidence)
            {
                return Err(LedgerError::DuplicateEvent);
            }
            if kind == EventKind::Spent {
                // Balance guard already applied in record_spent; re-check
                // here so direct record() misuse cannot go negative.
                let balance =
                    verify_history(&subject, history).ok_or(LedgerError::ReceiptInvalid)?;
                if balance < amount {
                    return Err(LedgerError::InsufficientBalance {
                        balance,
                        needed: amount,
                    });
                }
            }
        }
        let prev_head = history
            .last()
            .map(|c| c.head)
            .unwrap_or_else(|| genesis_head(&subject));
        let event = CreditEvent {
            subject,
            epoch,
            kind,
            amount,
            evidence,
        };
        let head = chain_head_after(prev_head, &event);
        history.push(ChainedEvent {
            event,
            prev_head,
            head,
        });
        let balance = verify_history(&subject, history).ok_or(LedgerError::ReceiptInvalid)?;
        Ok(balance)
    }
}
