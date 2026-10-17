//! Domain records and the sparse credit domain registry.

use std::collections::HashMap;

use pwb_core::crypto::{ContentId, PublicKey, VoucherSpendLog};
use pwb_core::system::is_reserved_system_domain;

use super::pricing::{PricingEpochStats, RollingPricing};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainRecord {
    pub domain: String,
    pub target_content_id: ContentId,
    pub owner: PublicKey,
    pub registered_at_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainRegistry {
    pub(super) records: HashMap<String, DomainRecord>,
    /// Credits already spent per owner via attested claims (prevents replaying
    /// one $k$-of-$N$ certificate across multiple domain claims).
    pub(super) claimed: HashMap<PublicKey, u64>,
    /// Redeemed blind-voucher tokens (prevents replaying one bearer voucher
    /// across multiple private claims).
    pub(super) spend_log: VoucherSpendLog,
    /// Dynamic claim price from rolling network metrics (§5).
    pricing: RollingPricing,
}

impl DomainRegistry {
    pub fn new() -> Self {
        DomainRegistry {
            records: HashMap::new(),
            claimed: HashMap::new(),
            spend_log: VoucherSpendLog::new(),
            pricing: RollingPricing::new(),
        }
    }

    /// Current price to claim a domain (credits) — the rolling pricing
    /// engine's answer, never a fixed constant (§5).
    pub fn claim_cost(&self) -> u64 {
        self.pricing.current_price()
    }

    /// Fold one epoch of network-wide stats into the rolling price.
    pub fn record_pricing_epoch(&mut self, stats: PricingEpochStats) {
        self.pricing.record_epoch(stats);
    }

    /// The pricing engine (read-only), e.g. for gossip aggregation.
    pub fn pricing(&self) -> &RollingPricing {
        &self.pricing
    }

    /// Merge a registry that evolved in isolation into this one (gossip
    /// partition merge). Deterministic CRDT semantics — commutative,
    /// associative and idempotent:
    ///
    /// * domains seen on only one side are kept;
    /// * conflicting claims for the same domain resolve to the highest
    ///   `registered_at_epoch` (last writer wins); ties break on the
    ///   lexicographic owner public key bytes;
    /// * per-owner claimed spend takes the maximum (a spend is never
    ///   undone);
    /// * redeemed voucher tokens are unioned, so a bearer token cannot be
    ///   redeemed again across the re-joined network;
    /// * rolling price observations are unioned into the shared window and
    ///   the base cost takes the maximum observed on either side.
    pub fn merge(&mut self, other: &DomainRegistry) {
        for (domain, record) in &other.records {
            let keep = match self.records.get(domain) {
                None => true,
                Some(existing) => {
                    record.registered_at_epoch > existing.registered_at_epoch
                        || (record.registered_at_epoch == existing.registered_at_epoch
                            && record.owner.0 < existing.owner.0)
                }
            };
            if keep {
                self.records.insert(domain.clone(), record.clone());
            }
        }
        for (owner, spent) in &other.claimed {
            let entry = self.claimed.entry(*owner).or_insert(0);
            if *spent > *entry {
                *entry = *spent;
            }
        }
        self.spend_log.merge(&other.spend_log);
        self.pricing.merge(&other.pricing);
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

    /// Credits this owner has already spent through attested claims.
    pub fn claimed_by(&self, owner: &PublicKey) -> u64 {
        self.claimed.get(owner).copied().unwrap_or(0)
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
