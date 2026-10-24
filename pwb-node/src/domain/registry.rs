//! Domain records and the sparse credit domain registry.

use std::collections::HashMap;

use pwb_core::crypto::{ContentId, PublicKey, Signature, VoucherSpendLog};
use pwb_core::discovery::unix_time;
use pwb_core::system::is_reserved_system_domain;

use super::pricing::{PricingEpochStats, RollingPricing};

/// The attested funds that paid for one claim: the certificate chain head
/// plus the balance it vouched for. Records that cite the same head spend
/// from the same attested state, so funding groups — not just per-owner
/// totals — are what merges reconcile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaimFunding {
    /// Certificate chain head that funded the claim.
    pub head: [u8; 32],
    /// Balance that head attested.
    pub balance: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainRecord {
    pub domain: String,
    pub target_content_id: ContentId,
    pub owner: PublicKey,
    /// Credits the owner signed off as spent on this claim (0 for records
    /// seeded locally without a signed claim).
    pub credits_spent: u64,
    /// Claim ordering epoch (unix seconds at claim time).
    pub registered_at_epoch: u64,
    /// Owner signature over the canonical claim statement — or over the
    /// funding-bound statement when `funding` is present. `None` for locally
    /// seeded records, which are never served to or accepted from peers.
    pub signature: Option<Signature>,
    /// Attested funds that paid for this claim. `Some` exactly for claims
    /// made against a balance certificate; plain and blind claims leave it
    /// `None` (the blind path is single-use by token instead).
    pub funding: Option<ClaimFunding>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainRegistry {
    pub(super) records: HashMap<String, DomainRecord>,
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
    /// * attested funding groups that overspend their cited balance are
    ///   repaired (see [`Self::repair_funding`]): the same certificate
    ///   cannot fund two surviving claims, even across partitions;
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
        self.repair_funding();
        self.spend_log.merge(&other.spend_log);
        self.pricing.merge(&other.pricing);
    }

    /// Evict attested records whose funding overspends its cited balance.
    ///
    /// Every record citing the same `(owner, funding.head)` spends from one
    /// attested state, so the group's live `credits_spent` must fit the
    /// cited balance. Two partitions can each accept a claim against the
    /// same certificate while disconnected; on rejoin the group no longer
    /// fits, and the lowest-priority records are evicted until it does.
    /// A second pass enforces the cumulative budget across heads (a later
    /// head's balance already includes earlier earnings, so the owner's
    /// total live spend must fit the largest cited balance). Both passes
    /// keep the oldest claims first — first-spent wins, like money — with
    /// ties broken by domain name. Unfunded records (plain and blind
    /// claims) never participate: blind spends are single-use by token,
    /// and plain claims draw on the local signed ledger instead.
    ///
    /// Budgets use the minimum cited balance in each head group and the
    /// maximum across heads: honest records citing one head always agree,
    /// so only an owner citing less than their own records harms
    /// themselves. This is a pure function of the record set, so merges
    /// stay commutative and idempotent. Returns the number of evicted
    /// records.
    pub fn repair_funding(&mut self) -> usize {
        // Group funded record names by (owner, head).
        let mut groups: HashMap<(PublicKey, [u8; 32]), Vec<String>> = HashMap::new();
        for (domain, record) in &self.records {
            if let Some(funding) = record.funding {
                groups
                    .entry((record.owner, funding.head))
                    .or_default()
                    .push(domain.clone());
            }
        }
        let mut evict: Vec<String> = Vec::new();
        for domains in groups.values_mut() {
            // First-spent wins: oldest claim keeps its funding.
            domains.sort_by(|a, b| {
                let ra = &self.records[a];
                let rb = &self.records[b];
                ra.registered_at_epoch
                    .cmp(&rb.registered_at_epoch)
                    .then_with(|| a.cmp(b))
            });
            let budget = domains
                .iter()
                .map(|domain| self.records[domain].funding.map(|f| f.balance).unwrap_or(0))
                .min()
                .unwrap_or(0);
            let mut spent = 0u64;
            for domain in domains {
                let cost = self.records[domain].credits_spent;
                if spent.saturating_add(cost) > budget {
                    evict.push(domain.clone());
                } else {
                    spent = spent.saturating_add(cost);
                }
            }
        }
        // Cumulative pass per owner across heads.
        let mut by_owner: HashMap<PublicKey, Vec<String>> = HashMap::new();
        for (domain, record) in &self.records {
            if record.funding.is_some() && !evict.contains(domain) {
                by_owner
                    .entry(record.owner)
                    .or_default()
                    .push(domain.clone());
            }
        }
        for (_, mut domains) in by_owner {
            domains.sort_by(|a, b| {
                let ra = &self.records[a];
                let rb = &self.records[b];
                ra.registered_at_epoch
                    .cmp(&rb.registered_at_epoch)
                    .then_with(|| a.cmp(b))
            });
            let budget = domains
                .iter()
                .map(|domain| self.records[domain].funding.map(|f| f.balance).unwrap_or(0))
                .max()
                .unwrap_or(0);
            let mut spent = 0u64;
            for domain in domains {
                let cost = self.records[&domain].credits_spent;
                if spent.saturating_add(cost) > budget {
                    evict.push(domain);
                } else {
                    spent = spent.saturating_add(cost);
                }
            }
        }
        let evicted = evict.len();
        for domain in evict {
            self.records.remove(&domain);
        }
        evicted
    }

    /// Register an unofficial user domain name (e.g. `mysite.pwb`).
    ///
    /// SECURITY RULE: The `.plain` TLD is the reserved official namespace (built-in system protocols plus browser-shipped content pages).
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
            credits_spent: 0,
            registered_at_epoch: unix_time(),
            signature: None,
            funding: None,
        };

        self.records.insert(clean_domain, record);
        Ok(())
    }

    /// Credits this owner has already spent through attested claims, derived
    /// from the live funded records rather than a side table. Because the
    /// funding travels on the records themselves, this accounting survives
    /// restarts (it is persisted with the registry) and converges on merge
    /// (one record counts once, however many partitions gossiped it).
    /// Plain and blind spends draw on other pots and are not counted here.
    pub fn claimed_by(&self, owner: &PublicKey) -> u64 {
        self.records
            .values()
            .filter(|record| record.owner == *owner && record.funding.is_some())
            .map(|record| record.credits_spent)
            .fold(0u64, |total, spent| total.saturating_add(spent))
    }

    /// Live attested spend against one funding head: the part of
    /// [`Self::claimed_by`] that cites `head`. A new claim citing `head`
    /// fits exactly when this plus its cost stays within the head's
    /// attested balance.
    pub fn funded_spend(&self, owner: &PublicKey, head: &[u8; 32]) -> u64 {
        self.records
            .values()
            .filter(|record| {
                record.owner == *owner && record.funding.map(|f| f.head) == Some(*head)
            })
            .map(|record| record.credits_spent)
            .fold(0u64, |total, spent| total.saturating_add(spent))
    }

    pub fn resolve(&self, domain: &str) -> Option<&DomainRecord> {
        self.records.get(&domain.trim().to_lowercase())
    }

    /// Whether `domain` is already registered (case-insensitive).
    pub fn contains(&self, domain: &str) -> bool {
        self.records.contains_key(&domain.trim().to_lowercase())
    }

    /// Apply one record using the same deterministic last-writer-wins rule as
    /// [`Self::merge`] (higher `registered_at_epoch` wins; ties break on the
    /// lexicographically smaller owner key). Returns `true` when the stored
    /// registry changed (i.e. this is new or supersedes the existing record).
    pub fn apply_record(&mut self, record: DomainRecord) -> bool {
        let domain = record.domain.trim().to_lowercase();
        let record = DomainRecord {
            domain: domain.clone(),
            ..record
        };
        let keep = match self.records.get(&domain) {
            None => true,
            Some(existing) => {
                record.registered_at_epoch > existing.registered_at_epoch
                    || (record.registered_at_epoch == existing.registered_at_epoch
                        && record.owner.0 < existing.owner.0)
            }
        };
        if keep && self.records.get(&domain) != Some(&record) {
            self.records.insert(domain, record);
            true
        } else {
            false
        }
    }

    /// A snapshot of every record, for persistence and registry sync.
    pub fn records_snapshot(&self) -> Vec<DomainRecord> {
        self.records.values().cloned().collect()
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
