//! Sparse Credit & Domain Name Registry (`pwb-node::domain`).
//!
//! Note: The `.plain` TLD is strictly reserved for built-in system protocols (`search.plain`,
//! `register.plain`, `gossip.plain`, `nodes.plain`). User network sites claim `.pwb` domain names.
//! The canonical reserved-domain list and `.plain` security rule live in
//! `pwb-core::system`; they are re-exported here for compatibility.

use pwb_core::crypto::{ContentId, PublicKey, VoucherSpendError, VoucherSpendLog};
use std::collections::{BTreeMap, HashMap, VecDeque};

use crate::blind::{BlindClaim, BlindClaimError};
use crate::ledger::{LedgerError, QuorumCertificate};

pub use pwb_core::system::{is_reserved_system_domain, RESERVED_SYSTEM_DOMAINS};

/// Price a fresh registry charges for the first claim (credits).
pub const INITIAL_CLAIM_PRICE: u64 = 50;

/// Per-epoch observations retained by [`RollingPricing`] (§5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PricingEpochStats {
    /// Epoch the stats cover (must strictly advance).
    pub epoch: u64,
    /// Domains claimed network-wide during the epoch (demand flow).
    pub registrations: u64,
    /// Credits minted network-wide during the epoch (credit velocity /
    /// work capacity).
    pub network_work: u64,
    /// Active earning nodes during the epoch (market breadth).
    pub node_count: u64,
}

/// How many per-epoch observations feed the rolling averages.
pub const PRICING_WINDOW: usize = 8;
/// Claim flow (registrations/epoch) that reads as balanced demand.
const TARGET_REGS_PER_EPOCH: u64 = 4;
/// Per-node credits minted per epoch at balanced conditions.
const BASELINE_WORK_PER_NODE: u64 = 1_000;
/// Node count at balanced conditions.
const BASELINE_NODES: u64 = 10;
/// Fixed-point scale: 1_000 = "balanced".
const PRICE_SCALE: u64 = 1_000;
/// Share of the gap to the pressure target closed per epoch.
const ADJUSTMENT_DIVISOR: u64 = 4;
/// Lower bound on the claim price (credits).
pub const MIN_CLAIM_PRICE: u64 = 10;
/// Upper bound on the claim price (credits).
pub const MAX_CLAIM_PRICE: u64 = 10_000;

/// Dynamic domain-claim price from a rolling window of network metrics
/// (TODO §5): demand = recent registration flow vs. a balanced target;
/// supply = recent network-wide credit minting split into per-node work
/// capacity × node breadth. Each epoch the price moves one
/// [`ADJUSTMENT_DIVISOR`]-th of the way toward
/// `price × demand / supply`, clamped to [`MIN_CLAIM_PRICE`..=`MAX_CLAIM_PRICE`].
/// All integer math, fully deterministic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollingPricing {
    history: VecDeque<PricingEpochStats>,
    price: u64,
    floor: u64,
    ceiling: u64,
}

impl RollingPricing {
    /// Engine starting at [`INITIAL_CLAIM_PRICE`] with the standard bounds.
    pub fn new() -> Self {
        Self::with_bounds(INITIAL_CLAIM_PRICE, MIN_CLAIM_PRICE, MAX_CLAIM_PRICE)
    }

    /// Engine with an explicit starting price and bounds (tests/simulations).
    pub fn with_bounds(price: u64, floor: u64, ceiling: u64) -> Self {
        RollingPricing {
            history: VecDeque::new(),
            price: price.clamp(floor, ceiling),
            floor,
            ceiling,
        }
    }

    /// The current claim price (credits).
    pub fn current_price(&self) -> u64 {
        self.price
    }

    /// Observations retained (≤ [`PRICING_WINDOW`]).
    pub fn history_len(&self) -> usize {
        self.history.len()
    }

    /// Fold one epoch of network stats into the rolling averages and step
    /// the price toward the pressure-adjusted target. Epochs that do not
    /// strictly advance the window are ignored (gossip may deliver late or
    /// duplicate reports).
    pub fn record_epoch(&mut self, stats: PricingEpochStats) {
        if let Some(last) = self.history.back() {
            if stats.epoch <= last.epoch {
                return;
            }
        }
        self.history.push_back(stats);
        while self.history.len() > PRICING_WINDOW {
            self.history.pop_front();
        }
        self.price = self.next_price();
    }

    fn next_price(&self) -> u64 {
        let n = self.history.len() as u64;
        let sum_regs: u64 = self
            .history
            .iter()
            .fold(0u64, |acc, s| acc.saturating_add(s.registrations));
        let sum_work: u64 = self
            .history
            .iter()
            .fold(0u64, |acc, s| acc.saturating_add(s.network_work));
        let sum_nodes: u64 = self
            .history
            .iter()
            .fold(0u64, |acc, s| acc.saturating_add(s.node_count));
        let avg_regs = sum_regs / n;
        let avg_work = sum_work / n;
        let avg_nodes = (sum_nodes / n).max(1);

        // Demand: claim arrivals vs the balanced target (PRICE_SCALE = balanced).
        let demand = avg_regs.saturating_mul(PRICE_SCALE) / TARGET_REGS_PER_EPOCH.max(1);
        // Supply: per-node work capacity × network breadth, normalized so a
        // balanced network also reads PRICE_SCALE. capacity × breadth =
        // (work/nodes)/W0 × nodes/N0 = work/(W0×N0) — node count enters the
        // price through both credit supply and market size.
        let work_per_node = avg_work / avg_nodes;
        let capacity = work_per_node.saturating_mul(PRICE_SCALE) / BASELINE_WORK_PER_NODE;
        let breadth = avg_nodes.saturating_mul(PRICE_SCALE) / BASELINE_NODES;
        let supply = (capacity.saturating_mul(breadth) / PRICE_SCALE).max(1);

        // Pressure > SCALE raises the price over epochs; < SCALE lowers it.
        let pressure = demand.saturating_mul(PRICE_SCALE) / supply;
        let target = self.price.saturating_mul(pressure) / PRICE_SCALE;
        let price = if target > self.price {
            self.price + (target - self.price) / ADJUSTMENT_DIVISOR
        } else {
            self.price - (self.price - target) / ADJUSTMENT_DIVISOR
        };
        price.clamp(self.floor, self.ceiling)
    }

    /// Merge another engine's epoch observations into this one (gossip
    /// partition merge). Shared epochs are merged per-metric (maximum wins)
    /// so re-gossiping the same window never inflates demand or supply, and
    /// the window keeps the most recent [`PRICING_WINDOW`] distinct epochs
    /// — the merged window is the weighted moving average of what both
    /// partitions observed. The base cost takes the maximum price observed
    /// on either side so a cost established in either partition is never
    /// silently lowered; the ¼-epoch drift resumes toward the
    /// merged-window target as new epochs arrive.
    ///
    /// Commutative, associative and idempotent: merging with oneself is a
    /// no-op, and merging A with B yields the same state as B with A.
    pub fn merge(&mut self, other: &RollingPricing) {
        let mut combined: BTreeMap<u64, PricingEpochStats> = BTreeMap::new();
        for stats in self.history.iter().chain(other.history.iter()) {
            combined
                .entry(stats.epoch)
                .and_modify(|cur| {
                    // Shared epochs keep the maximum observation per metric:
                    // a merge is idempotent, so re-gossiping the same window
                    // never inflates demand or supply.
                    cur.registrations = cur.registrations.max(stats.registrations);
                    cur.network_work = cur.network_work.max(stats.network_work);
                    cur.node_count = cur.node_count.max(stats.node_count);
                })
                .or_insert(*stats);
        }
        let merged: VecDeque<PricingEpochStats> = combined
            .into_iter()
            .rev()
            .take(PRICING_WINDOW)
            .map(|(_, stats)| stats)
            .rev()
            .collect();
        self.history = merged;
        self.price = self.price.max(other.price).clamp(self.floor, self.ceiling);
    }
}

impl Default for RollingPricing {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainRecord {
    pub domain: String,
    pub target_content_id: ContentId,
    pub owner: PublicKey,
    pub registered_at_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainRegistry {
    records: HashMap<String, DomainRecord>,
    /// Credits already spent per owner via attested claims (prevents replaying
    /// one $k$-of-$N$ certificate across multiple domain claims).
    claimed: HashMap<PublicKey, u64>,
    /// Redeemed blind-voucher tokens (prevents replaying one bearer voucher
    /// across multiple private claims).
    spend_log: VoucherSpendLog,
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

    /// Credits this owner has already spent through attested claims.
    pub fn claimed_by(&self, owner: &PublicKey) -> u64 {
        self.claimed.get(owner).copied().unwrap_or(0)
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
            reg.register_attested("search.plain", cid, &cert, 2, &neighborhood),
            Err(LedgerError::ReservedSystemDomain)
        );

        // k=2 of the 3-member neighborhood claims and records the spend at
        // the registry's current (initial) rolling price.
        assert_eq!(
            reg.register_attested("welcome.pwb", cid, &cert, 2, &neighborhood),
            Ok(())
        );
        assert_eq!(reg.claimed_by(&subject), INITIAL_CLAIM_PRICE);
        let record = reg.resolve("welcome.pwb").expect("registered");
        assert_eq!(record.owner, subject);
        assert_eq!(record.registered_at_epoch, 4);

        assert_eq!(
            reg.register_attested("welcome.pwb", cid, &cert, 2, &neighborhood),
            Err(LedgerError::DomainAlreadyRegistered)
        );

        // Replaying the same certificate cannot overdraw: 100 - 50 spent
        // covers a second 50-credit claim, but not a third.
        assert_eq!(
            reg.register_attested("second.pwb", cid, &cert, 2, &neighborhood),
            Ok(())
        );
        assert_eq!(
            reg.register_attested("third.pwb", cid, &cert, 2, &neighborhood),
            Err(LedgerError::InsufficientAttestedBalance {
                attested: 100,
                already_claimed: 100,
                needed: INITIAL_CLAIM_PRICE
            })
        );

        // A threshold above the certificate's quorum is refused.
        assert_eq!(
            reg.register_attested("fourth.pwb", cid, &cert, 4, &neighborhood),
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
            reg.register_attested("evil.pwb", cid, &cert, 2, &neighborhood),
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
            reg.register_attested("evil.pwb", cid, &with_stranger, 3, &neighborhood),
            Err(LedgerError::SignerNotInNeighborhood)
        );
    }

    fn stats(
        epoch: u64,
        registrations: u64,
        network_work: u64,
        node_count: u64,
    ) -> PricingEpochStats {
        PricingEpochStats {
            epoch,
            registrations,
            network_work,
            node_count,
        }
    }

    /// Balanced conditions: 4 claims/epoch, 10 nodes × 1_000 credits of work.
    fn equilibrium(epoch: u64) -> PricingEpochStats {
        stats(epoch, 4, 10_000, 10)
    }

    #[test]
    fn test_rolling_pricing_starts_at_initial_and_stays_at_equilibrium() {
        let mut pricing = RollingPricing::new();
        assert_eq!(pricing.current_price(), INITIAL_CLAIM_PRICE);
        for epoch in 1..=12 {
            pricing.record_epoch(equilibrium(epoch));
        }
        assert_eq!(
            pricing.current_price(),
            INITIAL_CLAIM_PRICE,
            "balanced demand and supply keep the price stable"
        );
        assert_eq!(pricing.history_len(), PRICING_WINDOW);
    }

    #[test]
    fn test_rolling_pricing_rises_with_demand_and_falls_with_glut() {
        let mut pricing = RollingPricing::new();

        // Hot market: registration flow far above the target, thin work
        // capacity. The price climbs every epoch until it hits the ceiling.
        let mut last = pricing.current_price();
        for epoch in 1..=6 {
            pricing.record_epoch(stats(epoch, 12, 1_000, 1));
            let price = pricing.current_price();
            assert!(
                price > last || price == MAX_CLAIM_PRICE,
                "epoch {epoch}: {price} should exceed {last} or be at the ceiling"
            );
            last = price;
        }
        assert!(last > INITIAL_CLAIM_PRICE * 2);

        // Cold market: zero claims, abundant minting. The price decays to
        // the floor.
        for epoch in 7..=40 {
            pricing.record_epoch(stats(epoch, 0, 100_000, 100));
        }
        assert_eq!(pricing.current_price(), MIN_CLAIM_PRICE);
    }

    #[test]
    fn test_rolling_pricing_clamps_to_bounds() {
        let mut pricing = RollingPricing::new();
        for epoch in 1..=40 {
            pricing.record_epoch(stats(epoch, u64::MAX / 4, 1, 1));
        }
        assert_eq!(pricing.current_price(), MAX_CLAIM_PRICE);

        let mut pricing = RollingPricing::new();
        for epoch in 1..=40 {
            pricing.record_epoch(stats(epoch, 0, u64::MAX / 4, u64::MAX / 4));
        }
        assert_eq!(pricing.current_price(), MIN_CLAIM_PRICE);
    }

    #[test]
    fn test_rolling_pricing_ignores_stale_or_duplicate_epochs() {
        let mut pricing = RollingPricing::new();
        pricing.record_epoch(equilibrium(5));
        let price = pricing.current_price();
        // Same or older epochs are dropped without moving the price or
        // growing the window.
        pricing.record_epoch(equilibrium(5));
        pricing.record_epoch(equilibrium(3));
        assert_eq!(pricing.current_price(), price);
        assert_eq!(pricing.history_len(), 1);
        // A newer epoch is accepted.
        pricing.record_epoch(stats(6, 40, 0, 1));
        assert_eq!(pricing.history_len(), 2);
        assert!(pricing.current_price() > price);
    }

    #[test]
    fn test_registry_claim_price_is_dynamic_not_a_constant() {
        let mut reg = DomainRegistry::new();
        let cid = ContentId([1; 32]);
        let subject = pwb_core::crypto::Keypair::generate().public;
        let keys: Vec<pwb_core::crypto::Keypair> = (0..3)
            .map(|_| pwb_core::crypto::Keypair::generate())
            .collect();

        assert_eq!(reg.claim_cost(), INITIAL_CLAIM_PRICE);

        // A hot epoch pushes the price far above the initial constant.
        for epoch in 1..=4 {
            reg.record_pricing_epoch(stats(epoch, 40, 1_000, 1));
        }
        let hot = reg.claim_cost();
        assert!(hot > INITIAL_CLAIM_PRICE);

        // The elevated price now gates the attested claim path: a 60-credit
        // balance that covered two claims at the initial price covers none.
        let (cert, neighborhood) = attest_quorum(subject, 1, 60, [3; 32], &keys);
        assert_eq!(
            reg.register_attested("expensive.pwb", cid, &cert, 2, &neighborhood),
            Err(LedgerError::InsufficientAttestedBalance {
                attested: 60,
                already_claimed: 0,
                needed: hot
            })
        );

        // A balance above the hot price still clears.
        let (rich, neighborhood) = attest_quorum(subject, 2, hot, [9; 32], &keys);
        assert_eq!(
            reg.register_attested("expensive.pwb", cid, &rich, 2, &neighborhood),
            Ok(())
        );
        assert_eq!(reg.claimed_by(&subject), hot);
    }

    #[test]
    fn test_domain_registry_merge_keeps_domains_unique_to_each_side() {
        let mut a = DomainRegistry::new();
        let mut b = DomainRegistry::new();
        let cid_a = ContentId([1; 32]);
        let cid_b = ContentId([2; 32]);
        let owner_a = PublicKey([3; 32]);
        let owner_b = PublicKey([4; 32]);
        assert!(a.register("alpha.pwb", cid_a, owner_a).is_ok());
        assert!(b.register("beta.pwb", cid_b, owner_b).is_ok());

        a.merge(&b);

        assert!(a.resolve("alpha.pwb").is_some());
        assert!(a.resolve("beta.pwb").is_some());
        assert_eq!(a.count(), 2);
    }

    #[test]
    fn test_domain_registry_merge_conflict_higher_epoch_wins() {
        let cid = ContentId([9; 32]);
        let alice = PublicKey([10; 32]);
        let bob = PublicKey([11; 32]);
        let mut a = DomainRegistry::new();
        let mut b = DomainRegistry::new();
        a.records.insert(
            "contested.pwb".into(),
            DomainRecord {
                domain: "contested.pwb".into(),
                target_content_id: cid,
                owner: alice,
                registered_at_epoch: 3,
            },
        );
        b.records.insert(
            "contested.pwb".into(),
            DomainRecord {
                domain: "contested.pwb".into(),
                target_content_id: cid,
                owner: bob,
                registered_at_epoch: 7,
            },
        );
        a.merge(&b);
        let record = a.resolve("contested.pwb").expect("merged record exists");
        assert_eq!(record.owner, bob);
        assert_eq!(record.registered_at_epoch, 7);
        assert_eq!(record.target_content_id, cid);
    }

    #[test]
    fn test_domain_registry_merge_same_epoch_tiebreaks_by_owner() {
        let cid = ContentId([9; 32]);
        let low = PublicKey([1; 32]);
        let high = PublicKey([255; 32]);

        // a holds the high-owner claim, b the low-owner claim, same epoch.
        let mut a = DomainRegistry::new();
        let mut b = DomainRegistry::new();
        a.records.insert(
            "tie.pwb".into(),
            DomainRecord {
                domain: "tie.pwb".into(),
                target_content_id: cid,
                owner: high,
                registered_at_epoch: 5,
            },
        );
        b.records.insert(
            "tie.pwb".into(),
            DomainRecord {
                domain: "tie.pwb".into(),
                target_content_id: cid,
                owner: low,
                registered_at_epoch: 5,
            },
        );
        a.merge(&b);
        assert_eq!(a.resolve("tie.pwb").unwrap().owner, low);

        // Symmetric: merging in the opposite order yields the same winner.
        let mut c = DomainRegistry::new();
        let mut d = DomainRegistry::new();
        c.records.insert(
            "tie.pwb".into(),
            DomainRecord {
                domain: "tie.pwb".into(),
                target_content_id: cid,
                owner: low,
                registered_at_epoch: 5,
            },
        );
        d.records.insert(
            "tie.pwb".into(),
            DomainRecord {
                domain: "tie.pwb".into(),
                target_content_id: cid,
                owner: high,
                registered_at_epoch: 5,
            },
        );
        d.merge(&c);
        assert_eq!(d.resolve("tie.pwb").unwrap().owner, low);
    }

    #[test]
    fn test_domain_registry_merge_claimed_takes_max_and_is_idempotent() {
        let owner = PublicKey([7; 32]);
        let mut a = DomainRegistry::new();
        let mut b = DomainRegistry::new();
        a.claimed.insert(owner, 50);
        b.claimed.insert(owner, 80);
        a.merge(&b);
        assert_eq!(a.claimed_by(&owner), 80);

        // Re-merging the same side is a no-op.
        let snapshot = a.clone();
        a.merge(&b);
        assert_eq!(a, snapshot);
    }

    #[test]
    fn test_rolling_pricing_merge_takes_max_price() {
        let mut hot = RollingPricing::new();
        for epoch in 1..=6 {
            hot.record_epoch(stats(epoch, 12, 1_000, 1));
        }
        let hot_price = hot.current_price();
        assert!(hot_price > INITIAL_CLAIM_PRICE);

        let mut cold = RollingPricing::new();
        for epoch in 1..=6 {
            cold.record_epoch(stats(epoch, 0, 100_000, 100));
        }
        let cold_price = cold.current_price();
        assert_eq!(cold_price, MIN_CLAIM_PRICE);

        hot.merge(&cold);
        assert_eq!(hot.current_price(), hot_price.max(cold_price));

        // The merged window contains both partitions' epoch-1 observations,
        // so a subsequent balanced epoch is priced against the combined
        // demand/supply and tempers the hot price.
        hot.record_epoch(stats(7, 4, 10_000, 10));
        let after = hot.current_price();
        assert!(after <= hot_price, "{after} should not exceed {hot_price}");
    }

    #[test]
    fn test_rolling_pricing_merge_unions_epoch_history() {
        let mut a = RollingPricing::new();
        a.record_epoch(stats(1, 4, 10_000, 10));
        a.record_epoch(stats(2, 8, 20_000, 10));
        let mut b = RollingPricing::new();
        b.record_epoch(stats(2, 2, 5_000, 5)); // shared epoch 2: merged, not duplicated
        b.record_epoch(stats(3, 0, 100_000, 100));
        let a_price = a.current_price();
        let b_price = b.current_price();

        a.merge(&b);
        assert_eq!(a.history_len(), 3); // epochs 1, 2, 3 across both partitions
        assert_eq!(a.current_price(), a_price.max(b_price));
    }

    #[test]
    fn test_domain_registry_merge_is_commutative_and_idempotent() {
        fn make_pair() -> (DomainRegistry, DomainRegistry) {
            let cid = ContentId([9; 32]);
            let alice = PublicKey([10; 32]);
            let bob = PublicKey([11; 32]);
            let mut a = DomainRegistry::new();
            let mut b = DomainRegistry::new();
            let _ = a.register("alpha.pwb", cid, alice);
            let _ = b.register("beta.pwb", cid, bob);
            a.records.insert(
                "contested.pwb".into(),
                DomainRecord {
                    domain: "contested.pwb".into(),
                    target_content_id: cid,
                    owner: alice,
                    registered_at_epoch: 3,
                },
            );
            b.records.insert(
                "contested.pwb".into(),
                DomainRecord {
                    domain: "contested.pwb".into(),
                    target_content_id: cid,
                    owner: bob,
                    registered_at_epoch: 7,
                },
            );
            a.claimed.insert(alice, 50);
            b.claimed.insert(bob, 80);
            a.record_pricing_epoch(stats(1, 40, 1_000, 1));
            b.record_pricing_epoch(stats(1, 0, 100_000, 100));
            (a, b)
        }

        let (a, b) = make_pair();

        // Commutative: A merged with B yields the same state as B with A.
        let mut a1 = a.clone();
        let b1 = b.clone();
        a1.merge(&b1);
        let a2 = a.clone();
        let mut b2 = b.clone();
        b2.merge(&a2);
        assert_eq!(a1, b2);

        // Idempotent: merging with oneself is a no-op.
        let mut a3 = a1.clone();
        a3.merge(&a1);
        assert_eq!(a3, a1);
    }
}
