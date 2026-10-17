//! Balance attestations and $k$-of-$N$ quorum certificates.

use std::collections::HashSet;

use pwb_core::crypto::{Keypair, PublicKey, Signature};

use super::error::LedgerError;
use crate::ledger::LEDGER_SIGN_PREFIX;

/// One attestor's signed statement of a subject's balance at an epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BalanceAttestation {
    /// Subject the statement is about.
    pub subject: PublicKey,
    /// Epoch the statement covers.
    pub epoch: u64,
    /// Attested cumulative balance.
    pub balance: u64,
    /// Attested subject chain head.
    pub head: [u8; 32],
    /// Attestor identity.
    pub attestor: PublicKey,
    /// Attestor signature over the canonical statement encoding.
    pub signature: Signature,
}

impl BalanceAttestation {
    /// Sign a balance statement with an attestor keypair.
    pub fn sign(
        attestor: &Keypair,
        subject: PublicKey,
        epoch: u64,
        balance: u64,
        head: [u8; 32],
    ) -> Self {
        let message = attest_sign_message(subject, epoch, balance, head);
        BalanceAttestation {
            subject,
            epoch,
            balance,
            head,
            attestor: attestor.public,
            signature: attestor.sign(&message),
        }
    }

    /// Verify the attestor signature over the canonical statement encoding.
    pub fn verify(&self) -> bool {
        let message = attest_sign_message(self.subject, self.epoch, self.balance, self.head);
        self.signature.verify(&self.attestor, &message)
    }
}

/// A $k$-of-$N$ quorum of balance attestations over one identical statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuorumCertificate {
    /// Subject whose balance is being vouched for.
    pub subject: PublicKey,
    /// Epoch the certificate covers.
    pub epoch: u64,
    /// unanimously attested balance.
    pub balance: u64,
    /// Unanimously attested chain head.
    pub head: [u8; 32],
    /// The attestors' individual signed statements.
    pub attestations: Vec<BalanceAttestation>,
}

impl QuorumCertificate {
    /// Assemble a certificate from attestations that must all agree on
    /// `(subject, epoch, balance, head)`. The first attestation defines the
    /// statement; any disagreement is [`LedgerError::AttestationMismatch`].
    pub fn from_attestations(attestations: Vec<BalanceAttestation>) -> Result<Self, LedgerError> {
        let first = attestations
            .first()
            .ok_or(LedgerError::QuorumTooSmall { have: 0, need: 1 })?;
        let (subject, epoch, balance, head) =
            (first.subject, first.epoch, first.balance, first.head);
        for attestation in &attestations[1..] {
            if attestation.subject != subject
                || attestation.epoch != epoch
                || attestation.balance != balance
                || attestation.head != head
            {
                return Err(LedgerError::AttestationMismatch);
            }
        }
        Ok(QuorumCertificate {
            subject,
            epoch,
            balance,
            head,
            attestations,
        })
    }

    /// Verify the quorum: at least `need` distinct attestors, every one a
    /// member of `neighborhood`, none of them the subject, and every
    /// attestation signature valid over this certificate's statement.
    pub fn verify(&self, need: usize, neighborhood: &[PublicKey]) -> Result<(), LedgerError> {
        if self.attestations.len() < need {
            return Err(LedgerError::QuorumTooSmall {
                have: self.attestations.len(),
                need,
            });
        }
        let members: HashSet<&PublicKey> = neighborhood.iter().collect();
        let mut seen: HashSet<PublicKey> = HashSet::new();
        for attestation in &self.attestations {
            if attestation.subject != self.subject
                || attestation.epoch != self.epoch
                || attestation.balance != self.balance
                || attestation.head != self.head
            {
                return Err(LedgerError::AttestationMismatch);
            }
            if attestation.attestor == self.subject {
                return Err(LedgerError::SelfAttestation);
            }
            if !members.contains(&attestation.attestor) {
                return Err(LedgerError::SignerNotInNeighborhood);
            }
            if !seen.insert(attestation.attestor) {
                return Err(LedgerError::DuplicateSigner);
            }
            if !attestation.verify() {
                return Err(LedgerError::BadAttestationSignature);
            }
        }
        Ok(())
    }
}

fn attest_sign_message(subject: PublicKey, epoch: u64, balance: u64, head: [u8; 32]) -> Vec<u8> {
    let mut msg = Vec::with_capacity(LEDGER_SIGN_PREFIX.len() + 56);
    msg.extend_from_slice(LEDGER_SIGN_PREFIX);
    msg.extend_from_slice(b"attest");
    msg.extend_from_slice(&subject.0);
    msg.extend_from_slice(&epoch.to_le_bytes());
    msg.extend_from_slice(&balance.to_le_bytes());
    msg.extend_from_slice(&head);
    msg
}
