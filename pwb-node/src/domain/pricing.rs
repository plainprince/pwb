//! Dynamic rolling claim pricing (§5).

use std::collections::{BTreeMap, VecDeque};

/// Price a fresh registry charges for the first claim (credits).
///
/// This is deliberately equal to [`MIN_CLAIM_PRICE`]: publishing one bundle
/// earns one index reward (10 credits), so a fresh node can claim a name for
/// the site it just hosted without first farming unrelated traffic. Dynamic
/// pricing raises the price as network demand grows.
pub const INITIAL_CLAIM_PRICE: u64 = 10;

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
