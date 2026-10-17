//! Attested and private (blind) domain-claim paths.

use pwb_core::crypto::{ContentId, PublicKey, VoucherSpendError};
use pwb_core::system::is_reserved_system_domain;

use super::registry::{DomainRecord, DomainRegistry};
use crate::blind::{BlindClaim, BlindClaimError};
use crate::ledger::{LedgerError, QuorumCertificate};

impl DomainRegistry {
    /// Claim a domain on `register.plain` backed by a $k$-of-$N$ attestor
    /// quorum (TODO §4.2): the owner's authoritative balance is what the
    /// neighborhood attested, never a self-declared figure. The price is the
    /// registry's current rolling claim price (§5); the certificate must
    /// verify against `neighborhood` at threshold `k` and cover the price in
    /// unclaimed attested balance, after which the spent amount is recorded
    /// so the same certificate cannot fund a second claim.
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
        if self.records.contains_key(&clean_domain) {
            return Err(LedgerError::DomainAlreadyRegistered);
        }
        certificate.verify(k, neighborhood)?;
        let cost = self.claim_cost();
        let already = self.claimed.get(&certificate.subject).copied().unwrap_or(0);
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
            registered_at_epoch: certificate.epoch,
        };
        self.records.insert(clean_domain, record);
        self.claimed.insert(certificate.subject, already + cost);
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
            registered_at_epoch: claim.binding.epoch,
        };
        self.records.insert(clean_domain, record);
        Ok(())
    }
}
