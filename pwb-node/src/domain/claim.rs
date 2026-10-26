//! Attested and private (blind) domain-claim paths.

use pwb_core::crypto::{ContentId, PublicKey, VoucherSpendError};
use pwb_core::system::{is_reserved_system_domain, is_unofficial_domain};

use super::registry::{ClaimFunding, DomainRecord, DomainRegistry};
use crate::blind::{BlindClaim, BlindClaimError};
use crate::ledger::{LedgerError, QuorumCertificate};

impl DomainRegistry {
    /// Claim a domain on `register.plain` backed by a $k$-of-$N$ attestor
    /// quorum (TODO §4.2): the owner's authoritative balance is what the
    /// neighborhood attested, never a self-declared figure. The price is the
    /// registry's current rolling claim price (§5); the certificate must
    /// verify against `neighborhood` at threshold `k` and cover the price
    /// in unclaimed attested balance. The record cites the certificate's
    /// `(head, balance)` as its funding, so the same attested state cannot
    /// fund two surviving claims — not even across partitions (merges
    /// repair overdrawn funding groups).
    pub fn register_attested(
        &mut self,
        domain: &str,
        target: ContentId,
        certificate: &QuorumCertificate,
        k: usize,
        neighborhood: &[PublicKey],
    ) -> Result<(), LedgerError> {
        let clean_domain = domain.trim().to_lowercase();
        if is_reserved_system_domain(&clean_domain) {
            return Err(LedgerError::ReservedSystemDomain);
        }
        if !is_unofficial_domain(&clean_domain) {
            return Err(LedgerError::UnknownTld);
        }
        if self.records.contains_key(&clean_domain) {
            return Err(LedgerError::DomainAlreadyRegistered);
        }
        certificate.verify(k, neighborhood)?;
        let cost = self.claim_cost();
        // Spendable balance is derived from the live funded records, so a
        // restarted node enforces the same budget as before the restart.
        let already = self.claimed_by(&certificate.subject);
        if already.saturating_add(cost) > certificate.balance {
            return Err(LedgerError::InsufficientAttestedBalance {
                attested: certificate.balance,
                already_claimed: already,
                needed: cost,
            });
        }
        let record = DomainRecord {
            domain: clean_domain.clone(),
            target_content_id: target,
            owner: certificate.subject,
            credits_spent: cost,
            registered_at_epoch: certificate.epoch,
            // The registry holds no owner key here, so this record is local
            // only; it is never served to peers until the owner signs it.
            signature: None,
            funding: Some(ClaimFunding {
                head: certificate.head,
                balance: certificate.balance,
            }),
        };
        self.records.insert(clean_domain, record);
        Ok(())
    }

    /// Claim a domain privately (§4.3) by redeeming a blind voucher instead
    /// of exposing an attested balance certificate. The registry verifies the
    /// issuer binding (its attestor must be a neighborhood member), the voucher
    /// against the bound denomination key, the zero-knowledge ownership proof,
    /// and the denomination against the current rolling claim price (§5),
    /// then records the token in its spend log so the same voucher cannot
    /// fund a second claim. The owner is the redeemer's *pseudonym* key —
    /// unlinkable to the earning identity that withdrew the voucher.
    pub fn register_blind(
        &mut self,
        domain: &str,
        target: ContentId,
        owner: PublicKey,
        claim: &BlindClaim,
        neighborhood: &[PublicKey],
    ) -> Result<(), BlindClaimError> {
        let clean_domain = domain.trim().to_lowercase();
        if is_reserved_system_domain(&clean_domain) {
            return Err(BlindClaimError::ReservedSystemDomain);
        }
        if !is_unofficial_domain(&clean_domain) {
            return Err(BlindClaimError::UnknownTld);
        }
        if self.records.contains_key(&clean_domain) {
            return Err(BlindClaimError::DomainAlreadyRegistered);
        }
        if !claim.binding.verify() {
            return Err(BlindClaimError::BadBinding);
        }
        if !neighborhood.contains(&claim.binding.attestor) {
            return Err(BlindClaimError::IssuerNotAttestor);
        }
        if !claim.voucher.verify(&claim.binding.voucher_key) {
            return Err(BlindClaimError::BadVoucher);
        }
        if !claim.voucher.verify_ownership(&claim.proof) {
            return Err(BlindClaimError::NoOwnershipProof);
        }
        let cost = self.claim_cost();
        if claim.voucher.amount < cost {
            return Err(BlindClaimError::Underfunded {
                voucher: claim.voucher.amount,
                cost,
            });
        }
        self.spend_log
            .redeem(&claim.voucher, &claim.binding.voucher_key)
            .map_err(|err| match err {
                VoucherSpendError::AlreadySpent => BlindClaimError::AlreadySpent,
                VoucherSpendError::InvalidVoucher => BlindClaimError::BadVoucher,
            })?;
        let record = DomainRecord {
            domain: clean_domain.clone(),
            target_content_id: target,
            owner,
            credits_spent: cost,
            registered_at_epoch: claim.binding.epoch,
            // As above: a blind voucher proves payment, not an owner signature.
            signature: None,
            // Blind spends are single-use by token, not by attested state.
            funding: None,
        };
        self.records.insert(clean_domain, record);
        Ok(())
    }
}
