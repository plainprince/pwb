//! Sparse Credit & Domain Name Registry (`pwb-node::domain`).
//!
//! Note: The `.plain` TLD is strictly reserved for built-in system protocols (`search.plain`,
//! `register.plain`, `gossip.plain`, `nodes.plain`). User network sites claim `.pwb` domain names.
//! The canonical reserved-domain list and `.plain` security rule live in
//! `pwb-core::system`; they are re-exported here for compatibility.

use pwb_core::crypto::{ContentId, PublicKey, VoucherSpendError, VoucherSpendLog};
use std::collections::HashMap;

use crate::blind::{BlindClaim, BlindClaimError};
use crate::ledger::{LedgerError, QuorumCertificate};

pub use pwb_core::system::{is_reserved_system_domain, RESERVED_SYSTEM_DOMAINS};

pub const DOMAIN_CLAIM_COST: u64 = 50;

#[derive(Debug, Clone)]
pub struct DomainRecord {
    pub domain: String,
    pub target_content_id: ContentId,
    pub owner: PublicKey,
    pub registered_at_epoch: u64,
}

pub struct DomainRegistry {
    records: HashMap<String, DomainRecord>,
    /// Credits already spent per owner via attested claims (prevents replaying
    /// one $k$-of-$N$ certificate across multiple domain claims).
    claimed: HashMap<PublicKey, u64>,
    /// Redeemed blind-voucher tokens (prevents replaying one bearer voucher
    /// across multiple private claims).
    spend_log: VoucherSpendLog,
}

impl DomainRegistry {
    pub fn new() -> Self {
        DomainRegistry {
            records: HashMap::new(),
            claimed: HashMap::new(),
            spend_log: VoucherSpendLog::new(),
        }
    }

    /// Register a user domain name (e.g. `welcome.pwb`, `mysite.pwb`).
    ///
    /// SECURITY RULE: The `.plain` TLD is strictly reserved for built-in system protocols.
    /// Attempts by network participants to claim `.plain` domains will be rejected!
    pub fn register(
        &mut self,
        domain: &str,
        target: ContentId,
        owner: PublicKey,
    ) -> Result<(), &'static str> {
        let clean_domain = domain.trim().to_lowercase();

        // 1. Enforce `.plain` TLD system reservation security rule
        if is_reserved_system_domain(&clean_domain) {
            return Err(
                "Reserved system domain: .plain TLD is restricted to built-in system protocols",
            );
        }

        // 2. Reject duplicates
        if self.records.contains_key(&clean_domain) {
            return Err("Domain name already registered");
        }

        let record = DomainRecord {
            domain: clean_domain.clone(),
            target_content_id: target,
            owner,
            registered_at_epoch: 1,
        };

        self.records.insert(clean_domain, record);
        Ok(())
    }

    /// Claim a domain on `register.plain` backed by a $k$-of-$N$ attestor
    /// quorum (TODO §4.2): the owner's authoritative balance is what the
    /// neighborhood attested, never a self-declared figure. The certificate
    /// must verify against `neighborhood` at threshold `k`, cover at least
    /// `cost` in unclaimed attested balance, and then the spent amount is
    /// recorded so the same certificate cannot fund a second claim.
    pub fn register_attested(
        &mut self,
        domain: &str,
        target: ContentId,
        certificate: &QuorumCertificate,
        cost: u64,
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

    /// Credits this owner has already spent through attested claims.
    pub fn claimed_by(&self, owner: &PublicKey) -> u64 {
        self.claimed.get(owner).copied().unwrap_or(0)
    }

    /// Claim a domain privately (§4.3) by redeeming a blind voucher instead
    /// of exposing an attested balance certificate. The registry verifies the
    /// issuer binding (its attestor must be a neighborhood member), the voucher
    /// against the bound denomination key, the zero-knowledge ownership proof,
    /// and the denomination against `cost`, then records the token in its
    /// spend log so the same voucher cannot fund a second claim. The owner is
    /// the redeemer's *pseudonym* key — unlinkable to the earning identity
    /// that withdrew the voucher.
    pub fn register_blind(
        &mut self,
        domain: &str,
        target: ContentId,
        owner: PublicKey,
        claim: &BlindClaim,
        cost: u64,
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

    pub fn resolve(&self, domain: &str) -> Option<&DomainRecord> {
        self.records.get(&domain.trim().to_lowercase())
    }

    pub fn count(&self) -> usize {
        self.records.len()
    }
}

impl Default for DomainRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_domain_registry_security_rules() {
        let mut reg = DomainRegistry::new();
        let cid = ContentId([1; 32]);
        let pk = PublicKey([2; 32]);

        // Attempting to claim .plain domain MUST fail!
        assert!(reg.register("search.plain", cid, pk).is_err());
        assert!(reg.register("malicious.plain", cid, pk).is_err());
        assert!(reg.register("hacked.plain", cid, pk).is_err());

        // Registering user domain .pwb MUST succeed!
        assert!(reg.register("welcome.pwb", cid, pk).is_ok());
        assert!(reg.register("welcome.pwb", cid, pk).is_err()); // duplicate check

        let resolved = reg.resolve("WELCOME.PWB").unwrap();
        assert_eq!(resolved.target_content_id, cid);
    }

    fn attest_quorum(
        subject: PublicKey,
        epoch: u64,
        balance: u64,
        head: [u8; 32],
        keys: &[pwb_core::crypto::Keypair],
    ) -> (crate::ledger::QuorumCertificate, Vec<PublicKey>) {
        let attestations = keys
            .iter()
            .map(|key| crate::ledger::BalanceAttestation::sign(key, subject, epoch, balance, head))
            .collect();
        let neighborhood: Vec<PublicKey> = keys.iter().map(|k| k.public).collect();
        (
            crate::ledger::QuorumCertificate::from_attestations(attestations).expect("quorum"),
            neighborhood,
        )
    }

    #[test]
    fn test_attested_claim_requires_valid_quorum_and_spends_balance() {
        let mut reg = DomainRegistry::new();
        let cid = ContentId([1; 32]);
        let subject = pwb_core::crypto::Keypair::generate().public;
        let keys: Vec<pwb_core::crypto::Keypair> = (0..3)
            .map(|_| pwb_core::crypto::Keypair::generate())
            .collect();
        let (cert, neighborhood) = attest_quorum(subject, 4, 100, [7; 32], &keys);

        // Reserved and duplicate checks apply on the attested path too.
        assert_eq!(
            reg.register_attested("search.plain", cid, &cert, 50, 2, &neighborhood),
            Err(LedgerError::ReservedSystemDomain)
        );

        // k=2 of the 3-member neighborhood claims and records the spend.
        assert_eq!(
            reg.register_attested("welcome.pwb", cid, &cert, 50, 2, &neighborhood),
            Ok(())
        );
        assert_eq!(reg.claimed_by(&subject), 50);
        let record = reg.resolve("welcome.pwb").expect("registered");
        assert_eq!(record.owner, subject);
        assert_eq!(record.registered_at_epoch, 4);

        assert_eq!(
            reg.register_attested("welcome.pwb", cid, &cert, 50, 2, &neighborhood),
            Err(LedgerError::DomainAlreadyRegistered)
        );

        // Replaying the same certificate cannot overdraw: 100 - 50 spent
        // covers a second 50-credit claim, but not a third.
        assert_eq!(
            reg.register_attested("second.pwb", cid, &cert, 50, 2, &neighborhood),
            Ok(())
        );
        assert_eq!(
            reg.register_attested("third.pwb", cid, &cert, 50, 2, &neighborhood),
            Err(LedgerError::InsufficientAttestedBalance {
                attested: 100,
                already_claimed: 100,
                needed: 50
            })
        );

        // A threshold above the certificate's quorum is refused.
        assert_eq!(
            reg.register_attested("fourth.pwb", cid, &cert, 1, 4, &neighborhood),
            Err(LedgerError::QuorumTooSmall { have: 3, need: 4 })
        );
    }

    #[test]
    fn test_attested_claim_rejects_forged_and_stranger_quorums() {
        let mut reg = DomainRegistry::new();
        let cid = ContentId([2; 32]);
        let subject = pwb_core::crypto::Keypair::generate().public;
        let keys: Vec<pwb_core::crypto::Keypair> = (0..2)
            .map(|_| pwb_core::crypto::Keypair::generate())
            .collect();
        let (mut cert, neighborhood) = attest_quorum(subject, 1, 60, [3; 32], &keys);

        // Inflating the envelope balance breaks attestation agreement.
        cert.balance = 6000;
        assert_eq!(
            reg.register_attested("evil.pwb", cid, &cert, 50, 2, &neighborhood),
            Err(LedgerError::AttestationMismatch)
        );

        // A stranger's signature does not count toward the quorum.
        let (cert, neighborhood) = attest_quorum(subject, 1, 60, [3; 32], &keys);
        let stranger = pwb_core::crypto::Keypair::generate();
        let mut with_stranger = cert.clone();
        with_stranger
            .attestations
            .push(crate::ledger::BalanceAttestation::sign(
                &stranger, subject, 1, 60, [3; 32],
            ));
        assert_eq!(
            reg.register_attested("evil.pwb", cid, &with_stranger, 50, 3, &neighborhood),
            Err(LedgerError::SignerNotInNeighborhood)
        );
    }
}
